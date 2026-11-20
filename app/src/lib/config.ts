import bs58 from "bs58";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";

const env = import.meta.env;
function required(name: string): string {
  const value = env[name] as string | undefined;
  if (!value) throw new Error(`${name} missing: run \`pnpm setup:devnet\` to create app/.env.local`);
  return value;
}

export const BASE_RPC = required("VITE_BASE_RPC");
export const ER_RPC = required("VITE_ER_RPC");
export const ROUTER_RPC = required("VITE_ROUTER_RPC");
export const VALIDATOR = new PublicKey(required("VITE_VALIDATOR"));
export const USDC_MINT = new PublicKey(required("VITE_USDC_MINT"));

export const base = new Connection(BASE_RPC, "confirmed");
export const er = new Connection(ER_RPC, "confirmed");

export type PlayerId = "a" | "b";
export const PLAYERS: Record<PlayerId, { label: string; keypair: Keypair }> = {
  a: { label: "Player A", keypair: Keypair.fromSecretKey(bs58.decode(required("VITE_PLAYER_A_SECRET"))) },
  b: { label: "Player B", keypair: Keypair.fromSecretKey(bs58.decode(required("VITE_PLAYER_B_SECRET"))) },
};

export const explorer = (sigOrAddress: string, cluster: "devnet" | "er" = "devnet") =>
  cluster === "er"
    ? `https://explorer.solana.com/${sigOrAddress.length > 50 ? "tx" : "address"}/${sigOrAddress}?cluster=custom&customUrl=${encodeURIComponent(ER_RPC)}`
    : `https://explorer.solana.com/${sigOrAddress.length > 50 ? "tx" : "address"}/${sigOrAddress}?cluster=devnet`;
