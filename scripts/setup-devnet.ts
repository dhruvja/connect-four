// Creates the devnet test-USDC mint and two funded player wallets, then writes app/.env.local.
// Idempotent: re-running only tops balances up.
import fs from "node:fs";
import path from "node:path";
import bs58 from "bs58";
import { LAMPORTS_PER_SOL, SystemProgram, Transaction, sendAndConfirmTransaction } from "@solana/web3.js";
import {
  createMint,
  getMint,
  getOrCreateAssociatedTokenAccount,
  mintTo,
} from "@solana/spl-token";
import { BASE_RPC, ER_RPC, ROOT, ROUTER_RPC, VALIDATOR, base, funder, localKeypair, mintKeypair } from "./config";

const SOL_PER_PLAYER = 0.3;
const USDC_PER_PLAYER = 1_000n * 1_000_000n;

async function main() {
  const payer = funder();
  const mint = mintKeypair();
  console.log("funder", payer.publicKey.toBase58(), (await base.getBalance(payer.publicKey)) / LAMPORTS_PER_SOL, "SOL");

  if (!(await base.getAccountInfo(mint.publicKey))) {
    await createMint(base, payer, payer.publicKey, null, 6, mint);
    console.log("created test USDC mint", mint.publicKey.toBase58());
  }
  await getMint(base, mint.publicKey);

  const players = { A: localKeypair("player-a"), B: localKeypair("player-b"), Stranger: localKeypair("stranger") };
  for (const [name, kp] of Object.entries(players)) {
    const target = name === "Stranger" ? 0.05 : SOL_PER_PLAYER;
    const balance = await base.getBalance(kp.publicKey);
    if (balance < (target * LAMPORTS_PER_SOL) / 2) {
      const tx = new Transaction().add(
        SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: kp.publicKey, lamports: Math.round(target * LAMPORTS_PER_SOL) - balance }),
      );
      await sendAndConfirmTransaction(base, tx, [payer]);
    }
    const tokenAccount = await getOrCreateAssociatedTokenAccount(base, payer, mint.publicKey, kp.publicKey);
    if (name !== "Stranger" && tokenAccount.amount < USDC_PER_PLAYER / 2n) {
      await mintTo(base, payer, mint.publicKey, tokenAccount.address, payer, USDC_PER_PLAYER - tokenAccount.amount);
    }
    const usdc = (await base.getTokenAccountBalance(tokenAccount.address)).value.uiAmountString;
    console.log(`Player ${name}`, kp.publicKey.toBase58(), (await base.getBalance(kp.publicKey)) / LAMPORTS_PER_SOL, "SOL,", usdc, "USDC");
  }

  const env = [
    `VITE_BASE_RPC=${BASE_RPC}`,
    `VITE_ER_RPC=${ER_RPC}`,
    `VITE_ROUTER_RPC=${ROUTER_RPC}`,
    `VITE_VALIDATOR=${VALIDATOR.toBase58()}`,
    `VITE_USDC_MINT=${mint.publicKey.toBase58()}`,
    `VITE_PLAYER_A_SECRET=${bs58.encode(players.A.secretKey)}`,
    `VITE_PLAYER_B_SECRET=${bs58.encode(players.B.secretKey)}`,
  ].join("\n");
  fs.writeFileSync(path.join(ROOT, "app/.env.local"), env + "\n", { mode: 0o600 });
  console.log("wrote app/.env.local");
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
