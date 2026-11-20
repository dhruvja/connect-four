# Security review — connect-four

Method: `solana-audit` skill (account/owner validation, PDA/bump, SPL token, CPI, lifecycle, DoS) plus
an independent review of the SDK macro output (`#[delegate]`, `#[commit]`, `#[ephemeral_accounts]`,
`#[ephemeral]`), the magic program's ephemeral-account rules and the delegation program's commit
whitelist. Scope: `programs/connect-four/src/*`.

## Findings and status

| ID | Severity | Finding | Status |
| --- | --- | --- | --- |
| H-1 | High | Anyone could pre-fund the predictable board/player PDA addresses so `start_game` fails forever on the ER, locking both wagers in the delegated game | **Fixed** — `abort_game` refunds both players once `START_TIMEOUT_SECS` (120 s) pass after the join with no board. Test: `squatted_board_address_cannot_lock_funds` |
| H-2 | High | The joiner could call `start_game` against an absent creator and win by turn timeout 60 s later | **Fixed** — only the creator can start. Tests: `start_game_rejects_non_players_and_double_start` + devnet "joiner starts" |
| H-3 | High | `validator` was unchecked; delegating to a non-MagicBlock key makes commit/undelegate impossible and locks funds | **Fixed** — `ALLOWED_VALIDATORS` allowlist (devnet Asia/EU/US + local). Test: `create_rejects_validator_outside_allowlist` |
| M-1 | Medium | Creator could cancel and re-create the same game id with a bigger wager before a join lands | **Fixed** — `join_game(expected_wager)`. Test: `join_rejects_changed_wager` |
| L-1 | Low | Any classic SPL mint is accepted; a mint with a freeze authority (incl. real USDC) can freeze an ATA and block `settle` for both players | **Accepted** — the test mint has no freeze authority. Before mainnet: pin the mint and/or switch to per-player claims |
| L-2 | Low | Vault ATA could be pre-created to make `create_game` fail | **Fixed** — `init_if_needed`. Test: `create_succeeds_when_vault_ata_was_precreated` |

Reviewed with nothing found: PDA seeds/canonical bumps, move authorization (player record keyed on the
signer), payout recipients (`has_one` on both players + mint, canonical ATAs), double settle/reinit/
re-delegation, injected magic accounts pinned by address, undelegation callback, draw split math,
Token-2022 (excluded by `Program<Token>`), ER vs base routing, sponsor funding margin.

## Residual risks

- **ER/validator liveness:** while a game is delegated, finishing it needs the MagicBlock ER. A
  long-term validator outage has no in-program rollback (the delegation program's timeout-rollback
  path is not wired in).
- **Squatting is still a griefing vector:** it can no longer lock funds, but it can force an abort.
  The attacker pays a tiny cost and both players get a full refund.
- **The ER clones a funded base account** (the precondition for H-1): MagicSVM does not model this,
  so the regression test injects the squatted account into the ER ledger directly.
