import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Mirrors rustdrop_core::transfer::TransferState. */
export type TransferState =
  | "WAITING_FOR_ACCEPT"
  | "TRANSFERRING"
  | "VERIFYING"
  | "COMPLETED"
  | "FAILED"
  | "CANCELLED";

export interface Me {
  name: string;
  id: string;
  fingerprint: string;
  downloads: string;
  version: string;
  error: string | null;
}

export interface Peer {
  id: string;
  name: string;
  platform: string;
  /** null while checking */
  trusted: boolean | null;
}

export interface Trusted {
  fingerprint: string;
  name: string;
}

export interface PairingOffer {
  link: string;
  size: number;
  /** size x size cells, row by row, true = dark */
  modules: boolean[];
}

export interface FileMeta {
  path: string;
  size: number;
}

export interface IncomingRequest {
  id: string;
  sender: string;
  platform: string;
  files: FileMeta[];
  count: number;
  total: number;
}

export interface Transfer {
  id: string;
  outgoing: boolean;
  peer: string;
  items: number;
  total: number;
  done: number;
  state: TransferState;
  reason: string | null;
  saved: string[] | null;
  startedAt: number;
  /** When bytes started moving; used for speed and time left. */
  flowingAt: number | null;
}

export type TransferUpdate = { id: string } & { [K in keyof Transfer]?: Transfer[K] | null };

export const api = {
  me: () => invoke<Me>("me"),
  peers: () => invoke<Peer[]>("peers"),
  trusted: () => invoke<Trusted[]>("trusted"),
  trust: (fingerprint: string, name: string) => invoke<void>("pair", { fingerprint, name }),
  untrust: (fingerprint: string) => invoke<void>("unpair", { fingerprint }),
  refresh: () => invoke<void>("refresh"),
  respond: (id: string, accept: boolean) => invoke<void>("respond", { id, accept }),
  send: (device: string, paths: string[]) => invoke<string>("send", { device, paths }),
  cancel: (id: string) => invoke<void>("cancel", { id }),
  startPairing: () => invoke<PairingOffer>("start_pairing"),
  stopPairing: () => invoke<void>("stop_pairing"),
  openDownloads: () => invoke<void>("open_downloads"),
  reveal: (path: string) => invoke<void>("reveal", { path }),
};

export const events = {
  peers: (f: (p: Peer[]) => void) => listen<Peer[]>("peers", (e) => f(e.payload)),
  incoming: (f: (r: IncomingRequest) => void) => listen<IncomingRequest>("incoming", (e) => f(e.payload)),
  transfer: (f: (u: TransferUpdate) => void) => listen<TransferUpdate>("transfer", (e) => f(e.payload)),
  fatal: (f: (message: string) => void) => listen<string>("fatal", (e) => f(e.payload)),
  paired: (f: (name: string) => void) => listen<string>("paired", (e) => f(e.payload)),
};

export const ACTIVE: TransferState[] = ["WAITING_FOR_ACCEPT", "TRANSFERRING", "VERIFYING"];
