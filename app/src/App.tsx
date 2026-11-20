import { useCallback, useEffect, useRef, useState } from "react";
import { PublicKey, type TransactionInstruction } from "@solana/web3.js";
import {
  type Board,
  COLS,
  DELEGATION_PROGRAM_ID,
  type EndReason,
  type Game,
  type GameResult,
  PROGRAM_ID,
  ROWS,
  START_TIMEOUT_SECS,
  TURN_TIMEOUT_SECS,
  abortGameIx,
  ata,
  cancelGameIx,
  createGameIx,
  delegateGameIx,
  errorMessage,
  fetchBoard,
  fetchGame,
  fetchGames,
  finishGameIx,
  formatUsdc,
  joinGameIx,
  playMoveIx,
  playerActionIx,
  send,
  settleIx,
  startGameIx,
} from "./lib/connectFour";
import { PLAYERS, type PlayerId, USDC_MINT, VALIDATOR, base, er, explorer } from "./lib/config";

// ───────────────────────────── shared hooks ─────────────────────────────

function usePoll(fn: () => Promise<void> | void, ms: number, deps: unknown[]) {
  const saved = useRef(fn);
  saved.current = fn;
  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setTimeout>;
    const tick = async () => {
      try {
        await saved.current();
      } catch (e) {
        console.warn(e);
      }
      if (alive) timer = setTimeout(tick, ms);
    };
    tick();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
}

function useNow() {
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now() / 1000), 500);
    return () => clearInterval(id);
  }, []);
  return now;
}

function useHashRoute(): [string | null, (game: string | null) => void] {
  const read = () => /^#\/game\/(\w+)/.exec(location.hash)?.[1] ?? null;
  const [game, setGame] = useState(read);
  useEffect(() => {
    const on = () => setGame(read());
    addEventListener("hashchange", on);
    return () => removeEventListener("hashchange", on);
  }, []);
  return [game, (g) => (location.hash = g ? `#/game/${g}` : "")];
}

const short = (k: PublicKey | string) => {
  const s = k.toString();
  return `${s.slice(0, 4)}…${s.slice(-4)}`;
};

type Tx = { label: string; sig: string; layer: "devnet" | "er" };

// ───────────────────────────── app ─────────────────────────────

export function App() {
  const as = new URLSearchParams(location.search).get("as") as PlayerId | null;
  if (!as || !PLAYERS[as]) return <Picker />;
  return <Shell me={as} />;
}

function Picker() {
  return (
    <main className="grid min-h-screen place-items-center p-6 text-slate-100">
      <div className="w-full max-w-sm space-y-4 text-center">
        <h1 className="text-3xl font-bold">Connect Four</h1>
        <p className="text-slate-400">Pick who plays in this window. Open the other player in a second window.</p>
        {(Object.keys(PLAYERS) as PlayerId[]).map((id) => (
          <a key={id} href={`?as=${id}`} target={id === "b" ? "_blank" : undefined} className="block rounded-xl bg-slate-800 px-4 py-3 font-semibold hover:bg-slate-700">
            Play as {PLAYERS[id].label} <span className="font-mono text-xs text-slate-400">{short(PLAYERS[id].keypair.publicKey)}</span>
          </a>
        ))}
      </div>
    </main>
  );
}

function Shell({ me }: { me: PlayerId }) {
  const wallet = PLAYERS[me].keypair;
  const [route, go] = useHashRoute();
  const [balances, setBalances] = useState<{ sol: number; usdc: bigint } | null>(null);
  const [txs, setTxs] = useState<Tx[]>([]);
  const record = useCallback((tx: Tx) => setTxs((t) => [tx, ...t].slice(0, 12)), []);

  usePoll(
    async () => {
      const [lamports, token] = await Promise.all([
        base.getBalance(wallet.publicKey),
        base.getTokenAccountBalance(ata(wallet.publicKey, USDC_MINT)).catch(() => null),
      ]);
      setBalances({ sol: lamports / 1e9, usdc: BigInt(token?.value.amount ?? 0) });
    },
    4000,
    [me],
  );

  return (
    <main className="mx-auto min-h-screen max-w-5xl p-4 text-slate-100 sm:p-6">
      <header className="mb-6 flex flex-wrap items-center justify-between gap-3">
        <button onClick={() => go(null)} className="text-left">
          <h1 className="text-2xl font-bold tracking-tight">
            Connect <span className="text-rose-400">Four</span>
          </h1>
          <p className="text-xs text-slate-400">Moves on a MagicBlock Ephemeral Rollup · wagers settled on Solana devnet</p>
        </button>
        <div className="rounded-xl bg-slate-900 px-4 py-2 text-sm ring-1 ring-slate-800">
          <span className={`mr-2 inline-block h-3 w-3 rounded-full ${me === "a" ? "bg-rose-500" : "bg-amber-400"}`} />
          <b>{PLAYERS[me].label}</b>{" "}
          <a className="font-mono text-slate-400 hover:underline" href={explorer(wallet.publicKey.toBase58())} target="_blank" rel="noreferrer">
            {short(wallet.publicKey)}
          </a>
          <span className="ml-3 text-slate-300">{balances ? `${formatUsdc(balances.usdc)} USDC · ${balances.sol.toFixed(3)} SOL` : "…"}</span>
        </div>
      </header>

      {route ? <GameView key={route} address={new PublicKey(route)} me={me} record={record} /> : <Lobby me={me} open={go} record={record} />}

      {txs.length > 0 && (
        <section className="mt-8 text-xs text-slate-400">
          <h3 className="mb-2 font-semibold uppercase tracking-wide text-slate-500">Recent transactions</h3>
          <ul className="space-y-1">
            {txs.map((t) => (
              <li key={t.sig}>
                <span className={`mr-2 rounded px-1.5 py-0.5 ${t.layer === "er" ? "bg-violet-900/60 text-violet-200" : "bg-emerald-900/60 text-emerald-200"}`}>
                  {t.layer === "er" ? "ER" : "Solana"}
                </span>
                {t.label}{" "}
                <a className="font-mono hover:underline" href={explorer(t.sig, t.layer)} target="_blank" rel="noreferrer">
                  {short(t.sig)}
                </a>
              </li>
            ))}
          </ul>
        </section>
      )}
    </main>
  );
}

// ───────────────────────────── lobby ─────────────────────────────

function Lobby({ me, open, record }: { me: PlayerId; open: (g: string) => void; record: (t: Tx) => void }) {
  const wallet = PLAYERS[me].keypair;
  const [wager, setWager] = useState("5");
  const [openGames, setOpenGames] = useState<Game[] | null>(null);
  const [mine, setMine] = useState<Game[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  usePoll(
    async () => {
      const [games, my] = await Promise.all([fetchGames(base, { status: "Open" }), fetchGames(base, { player: wallet.publicKey })]);
      setOpenGames(games.filter((g) => g.mint.equals(USDC_MINT)).sort((a, b) => b.createdAt - a.createdAt));
      setMine(my.filter((g) => g.status !== "Open"));
    },
    3000,
    [me],
  );

  async function run(label: string, fn: () => Promise<void>) {
    setBusy(label);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  }

  const create = () =>
    run("Creating game…", async () => {
      const amount = BigInt(Math.round(Number(wager) * 1e6));
      if (!(amount > 0n)) throw new Error("Enter a wager above 0");
      const id = BigInt(Date.now());
      const ix = createGameIx({ playerOne: wallet.publicKey, id, wager: amount, mint: USDC_MINT, validator: VALIDATOR });
      const sig = await send(base, [wallet], [ix]);
      record({ label: "create_game (wager escrowed)", sig, layer: "devnet" });
      open(ix.keys[1].pubkey.toBase58());
    });

  const join = (game: Game) =>
    run("Joining & delegating…", async () => {
      const sig = await send(base, [wallet], [joinGameIx(game, wallet.publicKey), delegateGameIx(game.address, wallet.publicKey)]);
      record({ label: "join_game + delegate_game", sig, layer: "devnet" });
      open(game.address.toBase58());
    });

  return (
    <div className="grid gap-6 md:grid-cols-2">
      <section className="rounded-2xl bg-slate-900 p-5 ring-1 ring-slate-800">
        <h2 className="mb-1 text-lg font-semibold">New game</h2>
        <p className="mb-4 text-sm text-slate-400">Your wager is escrowed on Solana. The opponent matches it; the winner takes both.</p>
        <label className="mb-3 flex items-center gap-2">
          <input
            value={wager}
            onChange={(e) => setWager(e.target.value)}
            inputMode="decimal"
            className="w-28 rounded-lg bg-slate-800 px-3 py-2 text-right font-mono ring-1 ring-slate-700 outline-none focus:ring-rose-400"
          />
          <span className="text-slate-300">USDC</span>
        </label>
        <button disabled={!!busy} onClick={create} className="w-full rounded-xl bg-rose-500 px-4 py-2.5 font-semibold text-white hover:bg-rose-400 disabled:opacity-50">
          {busy ?? "Create game"}
        </button>
        {error && <p className="mt-3 text-sm text-rose-300">{error}</p>}
      </section>

      <section className="rounded-2xl bg-slate-900 p-5 ring-1 ring-slate-800">
        <h2 className="mb-3 text-lg font-semibold">Open games</h2>
        {openGames === null && <p className="text-sm text-slate-400">Loading…</p>}
        {openGames?.length === 0 && <p className="text-sm text-slate-400">No open games. Create one, or create one from the other window.</p>}
        <ul className="space-y-2">
          {openGames?.map((g) => {
            const mineGame = g.playerOne.equals(wallet.publicKey);
            return (
              <li key={g.address.toBase58()} className="flex items-center justify-between rounded-xl bg-slate-800/70 px-3 py-2">
                <div>
                  <div className="font-semibold">{formatUsdc(g.wager)} USDC</div>
                  <div className="font-mono text-xs text-slate-400">
                    by {mineGame ? "you" : short(g.playerOne)} · {short(g.address)}
                  </div>
                </div>
                {mineGame ? (
                  <button onClick={() => open(g.address.toBase58())} className="rounded-lg bg-slate-700 px-3 py-1.5 text-sm hover:bg-slate-600">
                    Open
                  </button>
                ) : (
                  <button disabled={!!busy} onClick={() => join(g)} className="rounded-lg bg-amber-400 px-3 py-1.5 text-sm font-semibold text-slate-950 hover:bg-amber-300 disabled:opacity-50">
                    Join for {formatUsdc(g.wager)}
                  </button>
                )}
              </li>
            );
          })}
        </ul>
        {mine.length > 0 && (
          <>
            <h3 className="mt-5 mb-2 text-sm font-semibold text-slate-300">Your games awaiting settlement</h3>
            <ul className="space-y-2">
              {mine.map((g) => (
                <li key={g.address.toBase58()}>
                  <button onClick={() => open(g.address.toBase58())} className="w-full rounded-xl bg-slate-800/70 px-3 py-2 text-left text-sm hover:bg-slate-800">
                    {short(g.address)} · {g.status} · {formatUsdc(g.wager)} USDC
                  </button>
                </li>
              ))}
            </ul>
          </>
        )}
      </section>
    </div>
  );
}

// ───────────────────────────── game ─────────────────────────────

type Snapshot = {
  game: Game | null; // latest known game state (ER copy while delegated)
  delegated: boolean;
  board: Board | null;
  gone: boolean; // game account closed on Solana (settled or cancelled)
};

const RESULT_TEXT: Record<GameResult, string> = {
  InProgress: "",
  PlayerOneWins: "Player A wins",
  PlayerTwoWins: "Player B wins",
  Draw: "Draw",
};
const REASON_TEXT: Record<EndReason, string> = {
  None: "",
  ConnectFour: "four in a row",
  BoardFull: "board full",
  Timeout: "opponent timed out",
  Resignation: "resignation",
  Aborted: "never started — both refunded",
};

function GameView({ address, me, record }: { address: PublicKey; me: PlayerId; record: (t: Tx) => void }) {
  const wallet = PLAYERS[me].keypair;
  const now = useNow();
  const [snap, setSnap] = useState<Snapshot>({ game: null, delegated: false, board: null, gone: false });
  const [lastSeen, setLastSeen] = useState<Game | null>(null);
  // The board is an ER-only account that is closed when the game ends: keep the final position.
  const [lastBoard, setLastBoard] = useState<Board | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [hoverCol, setHoverCol] = useState<number | null>(null);
  const autoAt = useRef<Record<string, number>>({});

  const refresh = useCallback(async () => {
    const onBase = await fetchGame(base, address);
    if (!onBase) {
      setSnap((s) => ({ ...s, gone: true, board: null }));
      return;
    }
    const delegated = onBase.owner.equals(DELEGATION_PROGRAM_ID);
    let game = onBase.game;
    let board: Board | null = null;
    if (delegated) {
      const [onEr, b] = await Promise.all([fetchGame(er, address), fetchBoard(er, address)]);
      if (onEr && onEr.owner.equals(PROGRAM_ID)) game = onEr.game;
      board = b;
    }
    setSnap({ game, delegated, board, gone: false });
    setLastSeen(game);
    if (board) setLastBoard(board);
  }, [address]);

  usePoll(refresh, 700, [address.toBase58()]);

  const game = snap.game ?? lastSeen;
  const myPiece = game ? (game.playerOne.equals(wallet.publicKey) ? 1 : game.playerTwo.equals(wallet.publicKey) ? 2 : 0) : 0;
  const board = snap.board;

  // Phase of the lifecycle, derived from base + ER state.
  type Phase = "loading" | "open" | "delegating" | "starting" | "playing" | "ending" | "committing" | "settling" | "closed";
  let phase: Phase = "loading";
  if (snap.gone) phase = "closed";
  else if (!game) phase = "loading";
  else if (game.status === "Open") phase = "open";
  else if (game.status === "Finished") phase = snap.delegated ? "committing" : "settling";
  else if (!snap.delegated) phase = "delegating";
  else if (!board) phase = "starting";
  else if (board.result === "InProgress") phase = "playing";
  else phase = "ending";

  async function run(label: string, layer: "devnet" | "er", ixs: TransactionInstruction[], quiet = false) {
    if (!quiet) setBusy(label);
    setError(null);
    try {
      const sig = await send(layer === "er" ? er : base, [wallet], ixs);
      record({ label, sig, layer });
      await refresh();
    } catch (e) {
      if (!quiet) setError(errorMessage(e));
      else console.info(`${label}:`, errorMessage(e));
    } finally {
      if (!quiet) setBusy(null);
    }
  }

  /** Background lifecycle steps either window may push forward; retried every few seconds. */
  function auto(key: string, label: string, layer: "devnet" | "er", ixs: () => TransactionInstruction[]) {
    const last = autoAt.current[key] ?? 0;
    if (Date.now() - last < 4000) return;
    autoAt.current[key] = Date.now();
    void run(label, layer, ixs(), true);
  }

  useEffect(() => {
    if (!game || myPiece === 0) return;
    if (phase === "starting" && myPiece === 1) auto("start", "start_game (ephemeral board + players)", "er", () => [startGameIx(game, wallet.publicKey)]);
    if (phase === "ending") auto("finish", "finish_game (close ephemeral accounts, commit + undelegate)", "er", () => [finishGameIx(game, wallet.publicKey)]);
    if (phase === "settling") auto("settle", "settle (payout on Solana)", "devnet", () => [settleIx(game, wallet.publicKey)]);
  });

  if (phase === "loading" || !game) return <p className="text-slate-400">Loading game {short(address)}…</p>;

  const finalResult = game.result !== "InProgress" ? game.result : board?.result ?? "InProgress";
  const finalReason = game.endReason !== "None" ? game.endReason : board?.endReason ?? "None";
  const myTurn = phase === "playing" && board?.turn === myPiece;
  const deadline = board ? board.lastMoveTs + TURN_TIMEOUT_SECS : 0;
  const secondsLeft = Math.max(0, Math.ceil(deadline - now));
  const startDeadline = game.joinedAt + START_TIMEOUT_SECS;
  const pot = game.wager * 2n;

  const youWon = (finalResult === "PlayerOneWins" && myPiece === 1) || (finalResult === "PlayerTwoWins" && myPiece === 2);
  const statusLine: Record<Phase, string> = {
    loading: "",
    open: myPiece === 1 ? "Waiting for an opponent to join…" : "This game is open — join it from the lobby.",
    delegating: "Delegating the game to the Ephemeral Rollup…",
    starting: myPiece === 1 ? "Creating the ephemeral board on the ER…" : "Waiting for Player A to start the game…",
    playing: myTurn ? "Your move" : "Opponent's move",
    ending: `${RESULT_TEXT[finalResult]} (${REASON_TEXT[finalReason]}) · closing ephemeral accounts…`,
    committing: `${RESULT_TEXT[finalResult]} · committing result to Solana…`,
    settling: `${RESULT_TEXT[finalResult]} · paying out on Solana…`,
    closed: lastSeen?.status === "Open" ? "Game cancelled · wager refunded" : "Game settled on Solana",
  };

  return (
    <div className="grid gap-6 lg:grid-cols-[auto_1fr]">
      <div>
        <BoardView
          board={board ?? lastBoard}
          clickable={myTurn && !busy}
          hoverCol={hoverCol}
          setHoverCol={setHoverCol}
          myPiece={myPiece}
          onPlay={(c) => run(`play_move col ${c + 1}`, "er", [playMoveIx(game.address, wallet.publicKey, c)])}
        />
      </div>

      <aside className="space-y-4">
        <div className="rounded-2xl bg-slate-900 p-5 ring-1 ring-slate-800">
          <div className="mb-1 text-xs uppercase tracking-wide text-slate-500">
            Game <a className="font-mono hover:underline" href={explorer(game.address.toBase58())} target="_blank" rel="noreferrer">{short(game.address)}</a> · pot {formatUsdc(pot)} USDC
          </div>
          <div className={`text-xl font-semibold ${myTurn ? "text-emerald-300" : ""}`}>{statusLine[phase]}</div>
          {(phase === "closed" || phase === "settling" || phase === "committing") && finalResult !== "InProgress" && myPiece !== 0 && (
            <div className={`mt-2 text-lg ${finalResult === "Draw" ? "text-slate-200" : youWon ? "text-emerald-300" : "text-rose-300"}`}>
              {finalResult === "Draw"
                ? `Draw — ${formatUsdc(game.wager)} USDC back to each player`
                : youWon
                  ? `You won the ${formatUsdc(pot)} USDC pot (+${formatUsdc(game.wager)}) 🎉`
                  : `You lost ${formatUsdc(game.wager)} USDC`}
            </div>
          )}

          <div className="mt-4 grid grid-cols-2 gap-2 text-sm">
            <PlayerCard label="Player A" color="bg-rose-500" wallet={game.playerOne} you={myPiece === 1} active={phase === "playing" && board?.turn === 1} />
            <PlayerCard label="Player B" color="bg-amber-400" wallet={game.playerTwo} you={myPiece === 2} active={phase === "playing" && board?.turn === 2} />
          </div>

          {phase === "playing" && (
            <div className="mt-4 text-sm text-slate-300">
              Turn timer: <span className={`font-mono ${secondsLeft <= 10 ? "text-rose-300" : ""}`}>{secondsLeft}s</span>
              <span className="text-slate-500"> · after {TURN_TIMEOUT_SECS}s the waiting player can claim the win</span>
            </div>
          )}

          <div className="mt-4 flex flex-wrap gap-2">
            {phase === "open" && myPiece === 1 && (
              <Action busy={busy} onClick={() => run("cancel_game (refund)", "devnet", [cancelGameIx(game)])}>Cancel & refund</Action>
            )}
            {phase === "playing" && myPiece !== 0 && !myTurn && secondsLeft === 0 && (
              <Action busy={busy} primary onClick={() => run("claim_timeout", "er", [playerActionIx("claim_timeout", game.address, wallet.publicKey)])}>
                Claim win (opponent timed out)
              </Action>
            )}
            {phase === "playing" && myPiece !== 0 && (
              <Action busy={busy} onClick={() => confirm("Resign and lose your wager?") && run("resign", "er", [playerActionIx("resign", game.address, wallet.publicKey)])}>
                Resign
              </Action>
            )}
            {phase === "starting" && myPiece !== 0 && now >= startDeadline && (
              <Action busy={busy} onClick={() => run("abort_game (refund both)", "er", [abortGameIx(game.address, wallet.publicKey)])}>
                Abort & refund (not started)
              </Action>
            )}
            {phase === "starting" && myPiece === 2 && now < startDeadline && (
              <span className="text-sm text-slate-400">Abort available in {Math.ceil(startDeadline - now)}s if Player A doesn't start.</span>
            )}
            {(phase === "closed" || phase === "open") && (
              <button onClick={() => (location.hash = "")} className="rounded-xl bg-slate-800 px-4 py-2 text-sm hover:bg-slate-700">
                Back to lobby
              </button>
            )}
          </div>
          {busy && <p className="mt-3 text-sm text-slate-400">{busy}</p>}
          {error && <p className="mt-3 text-sm text-rose-300">{error}</p>}
        </div>

        <Lifecycle phase={phase} />
      </aside>
    </div>
  );
}

function PlayerCard({ label, color, wallet, you, active }: { label: string; color: string; wallet: PublicKey; you: boolean; active: boolean }) {
  const empty = wallet.equals(PublicKey.default);
  return (
    <div className={`rounded-xl px-3 py-2 ring-1 ${active ? "bg-slate-800 ring-emerald-400" : "bg-slate-800/50 ring-slate-800"}`}>
      <div className="flex items-center gap-2 font-semibold">
        <span className={`h-3 w-3 rounded-full ${color}`} />
        {label} {you && <span className="text-xs text-slate-400">(you)</span>}
      </div>
      <div className="font-mono text-xs text-slate-400">{empty ? "waiting…" : short(wallet)}</div>
    </div>
  );
}

function Action({ children, onClick, busy, primary }: { children: React.ReactNode; onClick: () => unknown; busy: string | null; primary?: boolean }) {
  return (
    <button
      disabled={!!busy}
      onClick={onClick}
      className={`rounded-xl px-4 py-2 text-sm font-semibold disabled:opacity-50 ${primary ? "bg-emerald-500 text-slate-950 hover:bg-emerald-400" : "bg-slate-800 hover:bg-slate-700"}`}
    >
      {children}
    </button>
  );
}

function BoardView(props: {
  board: Board | null;
  clickable: boolean;
  hoverCol: number | null;
  setHoverCol: (c: number | null) => void;
  myPiece: number;
  onPlay: (col: number) => void;
}) {
  const { board, clickable, hoverCol, setHoverCol, myPiece, onPlay } = props;
  const cells = board?.cells ?? Array(ROWS * COLS).fill(0);
  // Row index of the most recent piece, to animate only that one.
  const lastRow = board && board.lastColumn < COLS ? Math.max(...[...Array(ROWS).keys()].filter((r) => cells[r * COLS + board.lastColumn] !== 0)) : -1;
  const pieceColor = (v: number) => (v === 1 ? "bg-rose-500" : v === 2 ? "bg-amber-400" : "bg-slate-950");

  return (
    <div className="inline-block rounded-3xl bg-blue-700 p-3 shadow-2xl shadow-blue-950 sm:p-4" onMouseLeave={() => setHoverCol(null)}>
      <div className="grid grid-cols-7 gap-2 sm:gap-3">
        {[...Array(ROWS).keys()].reverse().map((r) =>
          [...Array(COLS).keys()].map((c) => {
            const v = cells[r * COLS + c];
            const animate = r === lastRow && c === board?.lastColumn;
            return (
              <button
                key={`${r}-${c}`}
                disabled={!clickable}
                onMouseEnter={() => setHoverCol(c)}
                onClick={() => onPlay(c)}
                className={`relative h-10 w-10 overflow-hidden rounded-full bg-slate-950 sm:h-14 sm:w-14 ${clickable ? "cursor-pointer" : "cursor-default"} ${clickable && hoverCol === c ? "ring-2 ring-white/40" : ""}`}
              >
                {v !== 0 && <span key={animate ? board?.moveCount : "static"} className={`absolute inset-0 rounded-full ${pieceColor(v)} ${animate ? "piece-drop" : ""} ${animate ? "ring-4 ring-white/60" : ""}`} />}
                {v === 0 && clickable && hoverCol === c && <span className={`absolute inset-1 rounded-full opacity-25 ${pieceColor(myPiece)}`} />}
              </button>
            );
          }),
        )}
      </div>
      {!board && <p className="mt-3 text-center text-sm text-blue-100">Board appears once the game starts on the Ephemeral Rollup</p>}
    </div>
  );
}

function Lifecycle({ phase }: { phase: string }) {
  const steps = [
    ["open", "Wager escrowed on Solana"],
    ["delegating", "Opponent joined · game delegated to ER"],
    ["starting", "Ephemeral board & player accounts created"],
    ["playing", "Moves executed on the ER"],
    ["ending", "Ephemeral accounts closed · result committed"],
    ["committing", "Game undelegated back to Solana"],
    ["settling", "Payout settled on Solana"],
  ];
  const order = [...steps.map((s) => s[0]), "closed"];
  const at = order.indexOf(phase);
  return (
    <ol className="rounded-2xl bg-slate-900 p-5 text-sm ring-1 ring-slate-800">
      {steps.map(([key, label], i) => (
        <li key={key} className={`flex items-center gap-2 py-0.5 ${i < at ? "text-slate-500" : i === at ? "font-semibold text-slate-100" : "text-slate-600"}`}>
          <span className="w-4">{i < at ? "✓" : i === at ? "▸" : "·"}</span>
          {label}
        </li>
      ))}
    </ol>
  );
}
