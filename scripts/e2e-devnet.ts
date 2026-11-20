// End-to-end verification on Solana devnet + MagicBlock devnet ER (Asia).
// Plays real games with the funded player wallets and checks every happy and unhappy path
// that can be exercised against live infrastructure.
//
// Usage: pnpm e2e:devnet [scenario...]   (scenarios: negatives win draw resign timeout cancel)
import { Keypair, PublicKey } from "@solana/web3.js";
import { getAccount, getAssociatedTokenAddressSync } from "@solana/spl-token";
import {
  PROGRAM_ID,
  type Game,
  boardPda,
  abortGameIx,
  cancelGameIx,
  createGameIx,
  delegateGameIx,
  delegationStatus,
  errorMessage,
  fetchBoard,
  fetchGame,
  finishGameIx,
  gamePda,
  joinGameIx,
  playMoveIx,
  playerActionIx,
  send,
  settleIx,
  startGameIx,
} from "../app/src/lib/connectFour";
import { ROUTER_RPC, VALIDATOR, base, er, localKeypair, mintKeypair } from "./config";

const WAGER = 2_000_000n; // 2 USDC
const DRAW = [3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 4, 4, 4, 4, 4, 4, 0, 1, 1, 1, 1, 1, 1, 5, 5, 5, 5, 5, 5, 0, 0, 0, 0, 0, 6, 6, 6, 6, 6, 6];

const A = localKeypair("player-a");
const B = localKeypair("player-b");
const stranger = localKeypair("stranger");
const mint = mintKeypair().publicKey;

let failures = 0;
const log = (...args: unknown[]) => console.log(new Date().toISOString().slice(11, 19), ...args);
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function check(cond: boolean, what: string) {
  if (cond) log("  ✔", what);
  else {
    failures++;
    log("  ✘ FAILED:", what);
  }
}

async function expectFail(what: string, p: Promise<unknown>, contains?: string) {
  try {
    await p;
    check(false, `${what} (unexpectedly succeeded)`);
  } catch (e) {
    const msg = errorMessage(e);
    check(!contains || msg.includes(contains), `${what} → rejected: ${msg}`);
  }
}

async function usdc(owner: PublicKey) {
  try {
    return (await getAccount(base, getAssociatedTokenAddressSync(mint, owner))).amount;
  } catch {
    return 0n;
  }
}

async function waitFor<T>(what: string, fn: () => Promise<T | null | false>, timeoutMs = 90_000): Promise<T> {
  const start = Date.now();
  for (;;) {
    const v = await fn().catch(() => null);
    if (v) return v;
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await sleep(1000);
  }
}

async function createGame(creator: Keypair) {
  const id = BigInt(Date.now());
  await send(base, [creator], [createGameIx({ playerOne: creator.publicKey, id, wager: WAGER, mint, validator: VALIDATOR })]);
  const game = (await fetchGame(base, gamePda(creator.publicKey, id)))!.game;
  log("created game", game.address.toBase58());
  return game;
}

/** Join + delegate on base, wait for the router/ER, creator starts on the ER. Returns the active game. */
async function joinAndStart(game: Game, joiner: Keypair, start = true): Promise<Game> {
  const sig = await send(base, [joiner], [joinGameIx(game, joiner.publicKey), delegateGameIx(game.address, joiner.publicKey)]);
  log("joined + delegated", sig);
  const status = await waitFor("router delegation", async () => {
    const s = await delegationStatus(ROUTER_RPC, game.address);
    return s.isDelegated ? s : null;
  });
  log("router: delegated, fqdn", status.fqdn);
  const active = await waitFor("game on ER", async () => {
    const g = await fetchGame(er, game.address);
    return g && g.owner.equals(PROGRAM_ID) ? g.game : null;
  });
  if (!start) return active;
  const startSig = await waitFor("start_game", () => send(er, [A], [startGameIx(active, A.publicKey)]), 30_000);
  log("started on ER", startSig);
  return active;
}

async function play(game: Game, moves: number[]) {
  const t0 = Date.now();
  for (let i = 0; i < moves.length; i++) {
    const who = i % 2 === 0 ? A : B;
    await send(er, [who], [playMoveIx(game.address, who.publicKey, moves[i])]);
  }
  log(`  ${moves.length} ER moves in ${Date.now() - t0} ms (avg ${Math.round((Date.now() - t0) / moves.length)} ms)`);
}

/** finish_game on the ER, wait for the commit + undelegation, settle on base and check payouts. */
async function finishAndSettle(game: Game, expected: { result: string; reason: string; a: bigint; b: bigint }, finish = true) {
  // Balances before the game's wagers were escrowed: wager already left both wallets.
  const before = { a: (await usdc(A.publicKey)) + WAGER, b: (await usdc(B.publicKey)) + WAGER };
  if (finish) {
    const finishSig = await send(er, [A], [finishGameIx(game, A.publicKey)]);
    log("finish_game on ER", finishSig);
  }
  check((await er.getAccountInfo(boardPda(game.address))) === null, "board ephemeral account closed on ER");
  const settled = await waitFor("game committed + undelegated to Solana", async () => {
    const g = await fetchGame(base, game.address);
    return g && g.owner.equals(PROGRAM_ID) && g.game.status === "Finished" ? g.game : null;
  });
  check(settled.result === expected.result && settled.endReason === expected.reason, `result committed to Solana: ${settled.result} / ${settled.endReason}`);
  const settleSig = await send(base, [stranger], [settleIx(settled, stranger.publicKey)]);
  log("settled on Solana by a third party", settleSig);
  const after = { a: await usdc(A.publicKey), b: await usdc(B.publicKey) };
  check(after.a - before.a === expected.a, `player A USDC delta ${after.a - before.a} == ${expected.a}`);
  check(after.b - before.b === expected.b, `player B USDC delta ${after.b - before.b} == ${expected.b}`);
  check((await base.getAccountInfo(game.address)) === null, "game account closed on Solana");
  await expectFail("settling twice", send(base, [stranger], [settleIx(settled, stranger.publicKey)]));
}

const scenarios: Record<string, () => Promise<void>> = {
  async negatives() {
    log("── negatives (base + ER)");
    await expectFail("zero wager", send(base, [A], [createGameIx({ playerOne: A.publicKey, id: 1n, wager: 0n, mint, validator: VALIDATOR })]), "Wager");
    const game = await createGame(A);
    await expectFail("creator joins own game", send(base, [A], [joinGameIx(game, A.publicKey)]), "own game");
    await expectFail("delegating an open game", send(base, [A], [delegateGameIx(game.address, A.publicKey)]), "not active");
    await expectFail("stranger cancels", send(base, [stranger], [cancelGameIx({ ...game, playerOne: stranger.publicKey })]));
    const active = await joinAndStart(game, B);
    await expectFail("stranger joins full game", send(base, [stranger], [joinGameIx(game, stranger.publicKey)]));
    await expectFail("creator cancels after join", send(base, [A], [cancelGameIx(game)]));
    await expectFail("stranger starts", send(er, [stranger], [startGameIx(active, stranger.publicKey)]), "creator");
    await expectFail("joiner starts", send(er, [B], [startGameIx(active, B.publicKey)]), "creator");
    await expectFail("double start", send(er, [A], [startGameIx(active, A.publicKey)]));
    await expectFail("player B moves first", send(er, [B], [playMoveIx(game.address, B.publicKey, 0)]), "not your turn");
    await expectFail("column 7", send(er, [A], [playMoveIx(game.address, A.publicKey, 7)]), "Column");
    await expectFail("stranger moves", send(er, [stranger], [playMoveIx(game.address, stranger.publicKey, 0)]));
    await expectFail("move sent to Solana instead of ER", send(base, [A], [playMoveIx(game.address, A.publicKey, 0)]));
    await expectFail("claim timeout too early", send(er, [B], [playerActionIx("claim_timeout", game.address, B.publicKey)]));
    await play(active, [0, 0, 0, 0, 0, 0]);
    await expectFail("full column", send(er, [A], [playMoveIx(game.address, A.publicKey, 0)]), "Column");
    await expectFail("finish while in progress", send(er, [A], [finishGameIx(active, A.publicKey)]), "in progress");
    await expectFail("settle while delegated", send(base, [stranger], [settleIx({ ...active, status: "Finished" }, stranger.publicKey)]));
    await play(active, [1, 2, 1, 2, 1, 2, 1]); // A connects column 1
    const board = await fetchBoard(er, game.address);
    check(board?.result === "PlayerOneWins", `board shows ${board?.result}`);
    await expectFail("move after game over", send(er, [B], [playMoveIx(game.address, B.publicKey, 3)]), "over");
    await finishAndSettle(active, { result: "PlayerOneWins", reason: "ConnectFour", a: WAGER, b: -WAGER });
  },
  async win() {
    log("── player B wins horizontally");
    const game = await joinAndStart(await createGame(A), B);
    await play(game, [6, 1, 6, 2, 6, 3, 5, 4]);
    await finishAndSettle(game, { result: "PlayerTwoWins", reason: "ConnectFour", a: -WAGER, b: WAGER });
  },
  async draw() {
    log("── full-board draw");
    const game = await joinAndStart(await createGame(A), B);
    await play(game, DRAW);
    await finishAndSettle(game, { result: "Draw", reason: "BoardFull", a: 0n, b: 0n });
  },
  async resign() {
    log("── player A resigns");
    const game = await joinAndStart(await createGame(A), B);
    await play(game, [3, 4]);
    await send(er, [A], [playerActionIx("resign", game.address, A.publicKey)]);
    await finishAndSettle(game, { result: "PlayerTwoWins", reason: "Resignation", a: -WAGER, b: WAGER });
  },
  async timeout() {
    log("── player B stalls, A claims after 60s");
    const game = await joinAndStart(await createGame(A), B);
    await play(game, [3]);
    await expectFail("B claims own timeout", send(er, [B], [playerActionIx("claim_timeout", game.address, B.publicKey)]));
    log("  waiting 61s for the turn timer…");
    await sleep(61_000);
    await send(er, [A], [playerActionIx("claim_timeout", game.address, A.publicKey)]);
    await finishAndSettle(game, { result: "PlayerOneWins", reason: "Timeout", a: WAGER, b: -WAGER });
  },
  async abort() {
    log("── creator never starts: joiner aborts after 120s, both refunded");
    const game = await joinAndStart(await createGame(A), B, false);
    await expectFail("abort before start timeout", send(er, [B], [abortGameIx(game.address, B.publicKey)]), "timeout");
    log("  waiting 121s for the start timer…");
    await sleep(121_000);
    await send(er, [B], [abortGameIx(game.address, B.publicKey)]);
    await finishAndSettle(game, { result: "Draw", reason: "Aborted", a: 0n, b: 0n }, false);
  },
  async cancel() {
    log("── create then cancel (no opponent)");
    const before = await usdc(A.publicKey);
    const game = await createGame(A);
    check((await usdc(A.publicKey)) === before - WAGER, "wager escrowed");
    await send(base, [A], [cancelGameIx(game)]);
    check((await usdc(A.publicKey)) === before, "wager refunded");
    check((await base.getAccountInfo(game.address)) === null, "game closed");
  },
};

async function main() {
  const selected = process.argv.slice(2);
  for (const name of selected.length ? selected : Object.keys(scenarios)) {
    try {
      await scenarios[name]();
    } catch (e) {
      failures++;
      log(`✘ scenario ${name} crashed:`, errorMessage(e), (e as { logs?: string[] }).logs?.slice(-8));
    }
  }
  log(failures === 0 ? "ALL DEVNET CHECKS PASSED" : `${failures} DEVNET CHECK(S) FAILED`);
  process.exit(failures ? 1 : 0);
}

main();
