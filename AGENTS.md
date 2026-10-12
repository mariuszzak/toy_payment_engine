# Engineering principles

Guidance for anyone, human or agent, changing this repository. Product behaviour and
the reasoning behind it live in [PRODUCT.md](PRODUCT.md); this file covers how we build.

## Architecture

- **Layered.** `domain/` is a pure core: money, identifiers, transactions, accounts and
  the ledger rules. It does no I/O and does not depend on `csv` or `serde`.
  `io/` converts bytes to validated transactions and accounts to CSV. `app/` streams
  rows through the ledger and reports diagnostics. `main.rs` only handles arguments,
  files, standard streams and exit codes.
- **Raw and validated types are separate.** `io::input::RawRecord` holds untrusted text.
  The reader checks each row's shape (line length, field count, encoding), then
  `TryFrom<RawRecord>` for `Transaction` validates every field. Checks that need ledger
  state (duplicates, unknown references, funds) belong to the ledger.
- **Invalid states are unrepresentable where practical.** A dispute has no amount field;
  `Amount` is always positive; account fields are private and change through one method.
- **Stream, never slurp.** Rows are read one at a time. The only state that grows is what
  the rules need: accounts and transaction records.

## Money

- Never use floating point. `f32` and `f64` are banned by `clippy.toml`.
- Money is fixed-point `i64` in units of 0.0001 (`Amount`, `Balance`).
- All arithmetic is checked. Overflow is a rejected transaction, never a wrap or a panic.
- Parse money from text directly; never round. Excess decimal places are accepted only
  when they are zeros.

## Errors and robustness

- The program never panics on input. Lints deny `unwrap`, `expect`, `panic!`, indexing,
  unchecked arithmetic, `as` casts and printing macros (which panic when a stream is
  closed) in production code. Tests may unwrap.
- Every lint exception is an `allow` attribute with a `reason = "..."`, enforced by
  `allow_attributes_without_reason`.
- Library errors use `thiserror`. `anyhow` is used only in `main.rs`.
- A row that cannot be applied is reported on stderr with its line number and a stable
  reason code, and processing continues. Only unreadable input, a bad header, a line
  over 1024 bytes, a bare carriage return or a failed write to stdout stop the run
  (exit code 1).
- stdout carries only the account CSV, written once after all input is read. stderr
  carries diagnostics; failures writing to stderr are ignored.
- Text from the input is never echoed raw: it is truncated and escaped.

## Tests

- Every ledger rule has a unit test. Every transaction type and parsing edge case has an
  end-to-end fixture under `tests/fixtures/`. Invariants have property tests.
- Name tests after behaviour (`withdrawal_exceeding_available_is_rejected`), structure
  them as Arrange / Act / Assert, share helpers through `tests/common`, and use `rstest`
  for tables.
- Assert on reason codes, not on message wording.
- Fixtures are written from scratch for this repository.

## Dependencies

- Approved: `csv`, `serde`, `thiserror`, `anyhow`, `proptest`, `rstest`.
- Anything else needs explicit approval with a short maturity and security assessment.
- Only use releases that are at least 7 days old. Versions are pinned exactly and
  `Cargo.lock` is committed.

## Toolchain

- Edition 2024, `rust-version = "1.85"`. Clippy uses `rust-version` to flag newer APIs.
- `just ci` runs formatting, lints, tests and the documentation build; run it before
  every commit.

## Repository hygiene

- Conventional Commits, one meaningful stage per commit, no co-author trailers.
- Never commit the original task brief, text derived from it, or the name of the
  organisation that issued it or its brands and products. This applies to code, docs,
  fixtures and commit messages. Check `git status` and the staged diff before committing.
