#!/usr/bin/env python3
"""Differential test: run the engine like a grader and compare with a reference model.

Generates a random batch of transactions, computes the expected account balances with
an independent model of the rules in PRODUCT.md, runs the release binary on the batch
and compares the two the way an automated grader would: row order, spacing and
trailing zeros are ignored.

Generated inputs are written under target/differential/, which is git-ignored.
Only the standard library is used.

    python3 scripts/differential_test.py --rows 200000 --seeds 1 2 3
"""

import argparse
import random
import subprocess
import sys
from decimal import Decimal
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUTPUT_DIR = REPO / "target" / "differential"
SCALE = 10_000  # units per whole amount: four decimal places
CLIENTS = 500
UNKNOWN_TX = 2**32 - 1
JUNK_LINES = [
    "",  # blank
    "   ",  # whitespace only
    "deposit,1,1,1,000.00",  # thousands separator: too many fields
    "Deposit,1,2,1",  # wrong letter case
    "deposit,1,3,1.23456",  # more than four decimal places
    "deposit,1,4,0",  # zero amount
]


def format_amount(rng, units):
    """Writes an amount with zero to four decimal places, sometimes without padding."""
    whole, fraction = divmod(units, SCALE)
    if fraction == 0 and rng.random() < 0.3:
        return str(whole)
    text = f"{whole}.{fraction:04d}"
    return text.rstrip("0") if fraction and rng.random() < 0.5 else text


def generate(rng, rows, chargeback_weight):
    """Returns CSV lines: movements with unique, unordered IDs, and dispute steps that
    mostly target recent movements, with some wrong clients, unknown IDs and junk."""
    lines = ["type, client, tx, amount"]
    movements = []  # (tx, client)
    fresh_ids = iter(rng.sample(range(1, UNKNOWN_TX), rows))
    for _ in range(rows):
        roll = rng.random()
        if roll < 0.55 or not movements:
            tx, client = next(fresh_ids), rng.randint(0, CLIENTS - 1)
            movements.append((tx, client))
            kind = "deposit" if roll < 0.35 or len(movements) == 1 else "withdrawal"
            amount = format_amount(rng, rng.randint(1, 5_000_000))
            separator = ", " if rng.random() < 0.5 else ","
            lines.append(separator.join([kind, str(client), str(tx), amount]))
        else:
            pool = movements[-50:] if rng.random() < 0.8 else movements
            tx, client = rng.choice(pool)
            if rng.random() < 0.05:
                client = rng.randint(0, CLIENTS - 1)
            if rng.random() < 0.03:
                tx = UNKNOWN_TX
            kind = rng.choices(["dispute", "resolve", "chargeback"], [5, 4, chargeback_weight])[0]
            trailing = "," if rng.random() < 0.5 else ""
            lines.append(f"{kind},{client},{tx}{trailing}")
        if rng.random() < 0.01:
            lines.append(rng.choice(JUNK_LINES))
    return lines


def parse_units(text):
    """Units for a plain decimal with at most four non-zero decimals, else None."""
    whole, _, fraction = text.partition(".")
    if not (whole + fraction).isdigit() or len(fraction.rstrip("0")) > 4:
        return None
    return int(whole or "0") * SCALE + int((fraction + "0000")[:4])


def expected_accounts(lines):
    """Reference model of the ledger rules, covering the input `generate` produces."""
    accounts, owners, deposits = {}, {}, {}

    def account(client):
        return accounts.setdefault(client, {"available": 0, "held": 0, "locked": False})

    for line in lines[1:]:
        fields = [field.strip() for field in line.split(",")]
        if not line.strip() or len(fields) > 4:
            continue
        kind = fields[0]
        if kind in ("deposit", "withdrawal"):
            units = parse_units(fields[3])
            client, tx = int(fields[1]), int(fields[2])
            if not units or tx in owners:
                continue
            owners[tx] = client
            balances = account(client)
            if balances["locked"]:
                continue
            if kind == "deposit":
                balances["available"] += units
                deposits[tx] = {"amount": units, "state": "settled"}
            elif balances["available"] >= units:
                balances["available"] -= units
        elif kind in ("dispute", "resolve", "chargeback"):
            client, tx = int(fields[1]), int(fields[2])
            if owners.get(tx) != client or tx not in deposits:
                continue
            deposit, balances = deposits[tx], account(client)
            units, state = deposit["amount"], deposit["state"]
            if kind == "dispute" and state in ("settled", "resolved"):
                balances["available"] -= units
                balances["held"] += units
                deposit["state"] = "disputed"
            elif kind == "resolve" and state == "disputed":
                balances["available"] += units
                balances["held"] -= units
                deposit["state"] = "resolved"
            elif kind == "chargeback" and state == "disputed":
                balances["held"] -= units
                balances["locked"] = True
                deposit["state"] = "charged_back"

    return {
        (
            client,
            Decimal(b["available"]) / SCALE,
            Decimal(b["held"]) / SCALE,
            Decimal(b["available"] + b["held"]) / SCALE,
            b["locked"],
        )
        for client, b in accounts.items()
    }


def engine_accounts(binary, path):
    """Runs the engine like a grader and parses its stdout, ignoring formatting."""
    result = subprocess.run([binary, path], capture_output=True, text=True, check=False)
    if result.returncode != 0:
        sys.exit(f"engine exited with {result.returncode}: {result.stderr.strip()}")
    rows = [line.split(",") for line in result.stdout.strip().splitlines()]
    header = [field.strip() for field in rows[0]]
    if header != ["client", "available", "held", "total", "locked"]:
        sys.exit(f"unexpected header: {header}")
    accounts = {
        (int(r[0]), Decimal(r[1]), Decimal(r[2]), Decimal(r[3]), r[4].strip() == "true")
        for r in rows[1:]
    }
    summary = next((l for l in result.stderr.splitlines() if l.startswith("summary:")), "")
    return accounts, summary


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--rows", type=int, default=200_000)
    parser.add_argument("--seeds", type=int, nargs="+", default=[1, 2, 3])
    parser.add_argument(
        "--chargeback-weight",
        type=float,
        default=2.0,
        help="relative weight of chargebacks among dispute steps (dispute 5, resolve 4)",
    )
    args = parser.parse_args()

    subprocess.run(["cargo", "build", "--quiet", "--release", "--locked"], cwd=REPO, check=True)
    binary = REPO / "target" / "release" / "payments-engine"
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    failures = 0
    for seed in args.seeds:
        lines = generate(random.Random(seed), args.rows, args.chargeback_weight)
        path = OUTPUT_DIR / f"seed-{seed}.csv"
        path.write_text("\n".join(lines) + "\n")
        actual, summary = engine_accounts(binary, path)
        expected = expected_accounts(lines)
        matches = actual == expected
        failures += not matches
        locked = sum(account[4] for account in actual)
        verdict = "MATCH" if matches else f"MISMATCH ({len(actual ^ expected)} rows differ)"
        print(f"seed {seed}: {verdict}; {len(actual)} accounts, {locked} locked; {summary}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
