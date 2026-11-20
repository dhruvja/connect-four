// Client for the connect-four program. Shared by the React UI and the devnet scripts.
// Builds instructions by hand from the IDL discriminators (no Anchor TS dependency).
import { Buffer } from "buffer";
import {
  Connection,
  Keypair,
  PublicKey,
  SystemProgram,
  Transaction,
  TransactionInstruction,
} from "@solana/web3.js";
import {
  ASSOCIATED_TOKEN_PROGRAM_ID,
  TOKEN_PROGRAM_ID,
  getAssociatedTokenAddressSync,
} from "@solana/spl-token";
import idl from "./idl.json";

export const PROGRAM_ID = new PublicKey(idl.address);
export const DELEGATION_PROGRAM_ID = new PublicKey("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
export const MAGIC_PROGRAM_ID = new PublicKey("Magic11111111111111111111111111111111111111");
export const MAGIC_CONTEXT_ID = new PublicKey("MagicContext1111111111111111111111111111111");
export const EPHEMERAL_VAULT_ID = new PublicKey("MagicVau1t999999999999999999999999999999999");

export const ROWS = 6;
export const COLS = 7;
export const TURN_TIMEOUT_SECS = 60;
export const START_TIMEOUT_SECS = 120;
export const USDC_DECIMALS = 6;

export type GameStatus = "Open" | "Active" | "Finished";
export type GameResult = "InProgress" | "PlayerOneWins" | "PlayerTwoWins" | "Draw";
export type EndReason = "None" | "ConnectFour" | "BoardFull" | "Timeout" | "Resignation" | "Aborted";
const STATUSES: GameStatus[] = ["Open", "Active", "Finished"];
const RESULTS: GameResult[] = ["InProgress", "PlayerOneWins", "PlayerTwoWins", "Draw"];
const REASONS: EndReason[] = ["None", "ConnectFour", "BoardFull", "Timeout", "Resignation", "Aborted"];

export type Game = {
  address: PublicKey;
  id: bigint;
  playerOne: PublicKey;
  playerTwo: PublicKey;
  mint: PublicKey;
  wager: bigint;
  validator: PublicKey;
  status: GameStatus;
  result: GameResult;
  endReason: EndReason;
  createdAt: number;
  joinedAt: number;
};

export type Board = {
  cells: number[]; // row-major, row 0 = bottom
  turn: 1 | 2;
  moveCount: number;
  lastColumn: number;
  lastMoveTs: number;
  result: GameResult;
  endReason: EndReason;
};

// ───────────────────────────── layouts ─────────────────────────────

const ixDisc = (name: string) => {
  const ix = idl.instructions.find((i) => i.name === name);
  if (!ix) throw new Error(`unknown instruction ${name}`);
  return Buffer.from(ix.discriminator);
};
const accountDisc = (name: string) => {
  const acc = idl.accounts.find((a) => a.name === name);
  if (!acc) throw new Error(`unknown account ${name}`);
  return Buffer.from(acc.discriminator);
};
export const GAME_DISCRIMINATOR = accountDisc("Game");
const BOARD_DISCRIMINATOR = accountDisc("Board");
/** Byte offsets inside the Game account (after the 8-byte discriminator). */
export const GAME_OFFSETS = { playerOne: 16, playerTwo: 48, status: 152 } as const;

const hasDiscriminator = (data: Buffer, disc: Buffer) => disc.every((b, i) => data[i] === b);
// DataView instead of Buffer bigint helpers: the browser `buffer` polyfill types them wrongly.
const view = (data: Buffer) => new DataView(data.buffer, data.byteOffset, data.byteLength);

export function decodeGame(address: PublicKey, data: Buffer): Game {
  if (!hasDiscriminator(data, GAME_DISCRIMINATOR)) throw new Error("not a Game account");
  let o = 8;
  const key = () => new PublicKey(data.subarray(o, (o += 32)));
  const u64 = () => ((o += 8), view(data).getBigUint64(o - 8, true));
  const id = u64();
  const playerOne = key();
  const playerTwo = key();
  const mint = key();
  const wager = u64();
  const validator = key();
  const status = STATUSES[data[o++]];
  const result = RESULTS[data[o++]];
  const endReason = REASONS[data[o++]];
  const createdAt = Number(view(data).getBigInt64(o, true));
  const joinedAt = Number(view(data).getBigInt64(o + 8, true));
  return { address, id, playerOne, playerTwo, mint, wager, validator, status, result, endReason, createdAt, joinedAt };
}

export function decodeBoard(data: Buffer): Board {
  if (!hasDiscriminator(data, BOARD_DISCRIMINATOR)) throw new Error("not a Board account");
  let o = 8 + 32;
  const cells = [...data.subarray(o, (o += ROWS * COLS))];
  const turn = data[o++] as 1 | 2;
  const moveCount = data[o++];
  const lastColumn = data[o++];
  const lastMoveTs = Number(view(data).getBigInt64(o, true));
  o += 8;
  return { cells, turn, moveCount, lastColumn, lastMoveTs, result: RESULTS[data[o]], endReason: REASONS[data[o + 1]] };
}

// ───────────────────────────── addresses ─────────────────────────────

const u64le = (n: bigint) => {
  const b = Buffer.alloc(8);
  new DataView(b.buffer, b.byteOffset, 8).setBigUint64(0, n, true);
  return b;
};
const pda = (seeds: (Buffer | Uint8Array)[], program = PROGRAM_ID) =>
  PublicKey.findProgramAddressSync(seeds, program)[0];

export const gamePda = (playerOne: PublicKey, id: bigint) =>
  pda([Buffer.from("game"), playerOne.toBuffer(), u64le(id)]);
export const boardPda = (game: PublicKey) => pda([Buffer.from("board"), game.toBuffer()]);
export const playerPda = (game: PublicKey, wallet: PublicKey) =>
  pda([Buffer.from("player"), game.toBuffer(), wallet.toBuffer()]);
export const ata = (owner: PublicKey, mint: PublicKey) =>
  getAssociatedTokenAddressSync(mint, owner, true);

// ───────────────────────────── instructions ─────────────────────────────

const meta = (pubkey: PublicKey, isWritable = false, isSigner = false) => ({ pubkey, isSigner, isWritable });
const ix = (name: string, keys: ReturnType<typeof meta>[], args: Buffer = Buffer.alloc(0)) =>
  new TransactionInstruction({ programId: PROGRAM_ID, keys, data: Buffer.concat([ixDisc(name), args]) });

export function createGameIx(p: { playerOne: PublicKey; id: bigint; wager: bigint; mint: PublicKey; validator: PublicKey }) {
  const game = gamePda(p.playerOne, p.id);
  return ix(
    "create_game",
    [
      meta(p.playerOne, true, true),
      meta(game, true),
      meta(p.mint),
      meta(ata(game, p.mint), true),
      meta(ata(p.playerOne, p.mint), true),
      meta(TOKEN_PROGRAM_ID),
      meta(ASSOCIATED_TOKEN_PROGRAM_ID),
      meta(SystemProgram.programId),
    ],
    Buffer.concat([u64le(p.id), u64le(p.wager), p.validator.toBuffer()]),
  );
}

export function cancelGameIx(game: Game) {
  return ix("cancel_game", [
    meta(game.playerOne, true, true),
    meta(game.address, true),
    meta(game.mint),
    meta(ata(game.address, game.mint), true),
    meta(ata(game.playerOne, game.mint), true),
    meta(TOKEN_PROGRAM_ID),
  ]);
}

/** `game.wager` is passed as the expected wager, so the join fails if the terms changed. */
export function joinGameIx(game: Game, playerTwo: PublicKey) {
  return ix(
    "join_game",
    [
      meta(playerTwo, true, true),
      meta(game.address, true),
      meta(game.mint),
      meta(ata(game.address, game.mint), true),
      meta(ata(playerTwo, game.mint), true),
      meta(TOKEN_PROGRAM_ID),
    ],
    u64le(game.wager),
  );
}

export function abortGameIx(game: PublicKey, payer: PublicKey) {
  return ix("abort_game", [
    meta(payer, true, true),
    meta(game, true),
    meta(boardPda(game)),
    meta(MAGIC_PROGRAM_ID),
    meta(MAGIC_CONTEXT_ID, true),
  ]);
}

export function delegateGameIx(game: PublicKey, payer: PublicKey) {
  return ix("delegate_game", [
    meta(payer, true, true),
    meta(pda([Buffer.from("buffer"), game.toBuffer()]), true),
    meta(pda([Buffer.from("delegation"), game.toBuffer()], DELEGATION_PROGRAM_ID), true),
    meta(pda([Buffer.from("delegation-metadata"), game.toBuffer()], DELEGATION_PROGRAM_ID), true),
    meta(game, true),
    meta(PROGRAM_ID),
    meta(DELEGATION_PROGRAM_ID),
    meta(SystemProgram.programId),
  ]);
}

export function startGameIx(game: Game, signer: PublicKey) {
  return ix("start_game", [
    meta(signer, false, true),
    meta(game.address, true),
    meta(boardPda(game.address), true),
    meta(playerPda(game.address, game.playerOne), true),
    meta(playerPda(game.address, game.playerTwo), true),
    meta(EPHEMERAL_VAULT_ID, true),
    meta(MAGIC_PROGRAM_ID),
  ]);
}

export function playMoveIx(game: PublicKey, signer: PublicKey, column: number) {
  return ix(
    "play_move",
    [meta(signer, false, true), meta(game), meta(boardPda(game), true), meta(playerPda(game, signer), true)],
    Buffer.from([column]),
  );
}

export function playerActionIx(name: "resign" | "claim_timeout", game: PublicKey, signer: PublicKey) {
  return ix(name, [meta(signer, false, true), meta(game), meta(boardPda(game), true), meta(playerPda(game, signer))]);
}

export function finishGameIx(game: Game, payer: PublicKey) {
  return ix("finish_game", [
    meta(payer, true, true),
    meta(game.address, true),
    meta(boardPda(game.address), true),
    meta(playerPda(game.address, game.playerOne), true),
    meta(playerPda(game.address, game.playerTwo), true),
    meta(EPHEMERAL_VAULT_ID, true),
    meta(MAGIC_PROGRAM_ID),
    meta(MAGIC_CONTEXT_ID, true),
  ]);
}

export function settleIx(game: Game, payer: PublicKey) {
  return ix("settle", [
    meta(payer, true, true),
    meta(game.address, true),
    meta(game.playerOne, true),
    meta(game.playerTwo),
    meta(game.mint),
    meta(ata(game.address, game.mint), true),
    meta(ata(game.playerOne, game.mint), true),
    meta(ata(game.playerTwo, game.mint), true),
    meta(TOKEN_PROGRAM_ID),
    meta(ASSOCIATED_TOKEN_PROGRAM_ID),
    meta(SystemProgram.programId),
  ]);
}

// ───────────────────────────── reads ─────────────────────────────

export async function fetchGame(connection: Connection, address: PublicKey) {
  const info = await connection.getAccountInfo(address, "confirmed");
  if (!info || info.data.length === 0) return null;
  return { game: decodeGame(address, Buffer.from(info.data)), owner: info.owner };
}

export async function fetchBoard(er: Connection, game: PublicKey): Promise<Board | null> {
  const info = await er.getAccountInfo(boardPda(game), "confirmed");
  if (!info || info.data.length === 0) return null;
  return decodeBoard(Buffer.from(info.data));
}

/** Games owned by the program on Solana (undelegated: Open or Finished-awaiting-settle). */
export async function fetchGames(connection: Connection, filters: { status?: GameStatus; player?: PublicKey } = {}) {
  const memcmp = [{ memcmp: { offset: 0, bytes: base58(GAME_DISCRIMINATOR) } }];
  if (filters.status) memcmp.push({ memcmp: { offset: GAME_OFFSETS.status, bytes: base58(Buffer.from([STATUSES.indexOf(filters.status)])) } });
  const fetchWith = async (extra?: { offset: number; bytes: string }) => {
    const accounts = await connection.getProgramAccounts(PROGRAM_ID, {
      commitment: "confirmed",
      filters: extra ? [...memcmp, { memcmp: extra }] : memcmp,
    });
    return accounts.map((a) => decodeGame(a.pubkey, Buffer.from(a.account.data)));
  };
  if (!filters.player) return fetchWith();
  const who = filters.player.toBase58();
  const [one, two] = await Promise.all([
    fetchWith({ offset: GAME_OFFSETS.playerOne, bytes: who }),
    fetchWith({ offset: GAME_OFFSETS.playerTwo, bytes: who }),
  ]);
  return [...one, ...two];
}

// ───────────────────────────── sending ─────────────────────────────

/** Sends and confirms a transaction. Throws with program logs on failure. */
export async function send(connection: Connection, signers: Keypair[], ixs: TransactionInstruction[]) {
  const tx = new Transaction().add(...ixs);
  const { blockhash, lastValidBlockHeight } = await connection.getLatestBlockhash("confirmed");
  tx.recentBlockhash = blockhash;
  tx.feePayer = signers[0].publicKey;
  tx.sign(...signers);
  const signature = await connection.sendRawTransaction(tx.serialize(), { skipPreflight: false, preflightCommitment: "confirmed" });
  const res = await connection.confirmTransaction({ signature, blockhash, lastValidBlockHeight }, "confirmed");
  if (res.value.err) {
    const detail = await connection.getTransaction(signature, { commitment: "confirmed", maxSupportedTransactionVersion: 0 });
    throw new ProgramError(JSON.stringify(res.value.err), detail?.meta?.logMessages ?? [], signature);
  }
  return signature;
}

export class ProgramError extends Error {
  constructor(message: string, public logs: string[], public signature?: string) {
    super(message);
  }
}

/** Human-readable reason from a failed send (custom program error message if present). */
export function errorMessage(e: unknown): string {
  const err = e as { logs?: string[]; transactionLogs?: string[]; message?: string };
  const logs = err.logs ?? err.transactionLogs ?? [];
  const anchorMsg = logs.map((l) => /Error Message: (.*)$/.exec(l)?.[1]).find(Boolean);
  if (anchorMsg) return anchorMsg.replace(/\.$/, "");
  const custom = /custom program error: (0x[0-9a-f]+)/i.exec(err.message ?? "")?.[1];
  if (custom) {
    const code = parseInt(custom, 16);
    const known = idl.errors.find((x) => x.code === code);
    if (known) return known.msg;
  }
  return err.message ?? String(e);
}

const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
function base58(bytes: Uint8Array): string {
  let n = BigInt("0x" + (Buffer.from(bytes).toString("hex") || "0"));
  let out = "";
  while (n > 0n) {
    out = ALPHABET[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const b of bytes) {
    if (b !== 0) break;
    out = "1" + out;
  }
  return out;
}

/** Router lookup: is this account delegated, and which ER serves it? */
export async function delegationStatus(routerUrl: string, account: PublicKey) {
  const res = await fetch(routerUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getDelegationStatus", params: [account.toBase58()] }),
  });
  const body = (await res.json()) as { result?: { isDelegated: boolean; fqdn?: string }; error?: { message: string } };
  if (body.error) throw new Error(body.error.message);
  return body.result ?? { isDelegated: false };
}

export const formatUsdc = (amount: bigint | number) => (Number(amount) / 10 ** USDC_DECIMALS).toLocaleString(undefined, { maximumFractionDigits: 2 });
