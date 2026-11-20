# Connect Four on MagicBlock

Two-player Connect Four with USDC wagers. Wagers are escrowed and paid out on **Solana devnet**; the
game itself (board, players, every move) runs on a **MagicBlock Ephemeral Rollup** using
**ephemeral accounts** that exist only on the ER and are closed when the game ends.

- Program: `9bji6FPUqL4E6CX7cdNAebFjT8CtFoAybP6jgXoud3GJ` (devnet)
- ER: devnet Asia `https://devnet-as.magicblock.app` (validator `MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57`)
- Test USDC mint (6 decimals): `2jfdjQ8Hx2yrf4dxApLoBUE2AiDjupMPzyW2dvU1Jsu4` — mint authority is `~/.config/solana/id.json`

## Play

```sh
pnpm install
pnpm dev                     # http://localhost:5173
```

Open two windows: `http://localhost:5173/?as=a` (Player A) and `http://localhost:5173/?as=b`
(Player B). Both wallets are pre-funded devnet burners (≈0.3 SOL, ≈1000 test USDC) whose keys live
in `keys/` and `app/.env.local` (git-ignored). A creates a game with a wager, B joins it from the
lobby, A's window starts it on the ER, then click a column to drop a piece.

Top up / recreate wallets any time: `pnpm setup:devnet` (idempotent).

## How it works

| Account | Where it lives | Lifecycle |
| --- | --- | --- |
| `Game` PDA `["game", creator, id]` | Solana; delegated to the ER while playing | Escrow terms + result. Created on Solana, delegated on join, committed + undelegated with the result, closed on settle |
| Vault (ATA of `Game`) | Solana only | Holds both wagers; emptied and closed on settle/cancel |
| `Board` PDA `["board", game]` | **ER only (ephemeral account)** | Created by `start_game`, closed by `finish_game` |
| `Player` PDA `["player", game, wallet]` ×2 | **ER only (ephemeral account)** | Created by `start_game`, closed by `finish_game` |

The delegated `Game` PDA is the rent **sponsor** for the three ephemeral accounts (the creator adds
100k lamports to it; all of it comes back).

| Step | Layer | Instruction |
| --- | --- | --- |
| Create game, escrow wager | Solana | `create_game(id, wager, validator)` (validator must be an allow-listed MagicBlock ER) |
| Cancel before anyone joins | Solana | `cancel_game` (refund) |
| Join + delegate (one tx) | Solana | `join_game(expected_wager)` + `delegate_game` |
| Start | ER | `start_game` — creator only; creates the ephemeral board + players |
| Play | ER | `play_move(column)`, `resign`, `claim_timeout` (opponent idle ≥ 60 s) |
| Never started | ER | `abort_game` — either player, ≥ 120 s after join; refunds both |
| End | ER | `finish_game` — closes ephemeral accounts, commits + undelegates `Game` to Solana |
| Settle | Solana | `settle` — permissionless; pays winner (or splits a draw), closes vault + game |

The UI pushes the lifecycle automatically: the creator's window starts the game, and whichever window
sees the result first calls `finish_game` and then `settle`.

## Verification

| Suite | Command | Result |
| --- | --- | --- |
| Board logic unit tests | `cargo test --lib` | 2/2 pass |
| MagicSVM (base + ER dual ledger, real `.so`) | `pnpm test:svm` | **34/34 pass** — 10 happy paths, 18 unhappy paths, 6 audit regressions |
| Live devnet + ER end-to-end | `pnpm e2e:devnet` | **all checks pass**: win, horizontal/vertical, draw (42 moves), resign, timeout, abort, cancel + 20 negative checks |
| Browser, two windows | Playwright against `pnpm dev` | full game, cancel, resign, timeout claim; no console errors |

On devnet an ER move confirms in ~150–220 ms; commit + undelegation back to Solana takes ~1–2 s.

## Build / deploy

```sh
anchor build --arch v1        # devnet currently rejects the default SBPF v3 output
cp target/idl/connect_four.json app/src/lib/idl.json
solana program deploy target/deploy/connect_four.so --program-id target/deploy/connect_four-keypair.json -u devnet
```

`tests-svm/` is a standalone crate (own lockfile + Rust 1.93) so MagicSVM's Solana crates don't
clash with the program's Anchor tree. Security review: see [AUDIT.md](AUDIT.md).
