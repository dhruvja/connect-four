import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const KEYS_DIR = path.join(ROOT, "keys");

export const BASE_RPC = "https://rpc.magicblock.app/devnet";
export const ER_RPC = "https://devnet-as.magicblock.app";
export const ROUTER_RPC = "https://devnet-router.magicblock.app";
/** Asia devnet ER validator identity (from getIdentity on ER_RPC). */
export const VALIDATOR = new PublicKey("MAS1Dt9qreoRMQ14YQuhg8UTZMMzDdKhmkZMECCzk57");

export const base = new Connection(BASE_RPC, "confirmed");
export const er = new Connection(ER_RPC, "confirmed");

export function readKeypair(file: string): Keypair {
  return Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(file, "utf8"))));
}

/** Loads keys/<name>.json, creating it if missing. */
export function localKeypair(name: string): Keypair {
  const file = path.join(KEYS_DIR, `${name}.json`);
  if (fs.existsSync(file)) return readKeypair(file);
  fs.mkdirSync(KEYS_DIR, { recursive: true });
  const kp = Keypair.generate();
  fs.writeFileSync(file, JSON.stringify([...kp.secretKey]), { mode: 0o600 });
  return kp;
}

export const funder = () => readKeypair(path.join(os.homedir(), ".config/solana/id.json"));
export const mintKeypair = () => localKeypair("usdc-mint");
