import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Mirrors rustdrop_core::transfer::TransferState. */
export type TransferState =
  | "WAITING_FOR_ACCEPT"
  | "TRANSFERRING"
  | "VERIFYING"
  | "COMPLETED"
  | "FAILED"
  | "CANCELLED"
  | "PAUSED";

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
  /** snake_case: TrustedInfo has no serde rename */
  auto_accept: boolean;
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
  /** First file or folder name. */
  name: string;
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

/** A finished or paused transfer from the core's history. `finished` is Unix seconds. */
export interface HistoryEntry {
  id: string;
  outgoing: boolean;
  peer: string;
  name: string;
  items: number;
  total: number;
  state: TransferState;
  reason: string | null;
  saved: string[];
  finished: number;
}

/** A link or text another device sent on purpose. */
export interface ReceivedText {
  text: string;
  from: string;
  at: number;
}

export const api = {
  me: () => invoke<Me>("me"),
  peers: () => invoke<Peer[]>("peers"),
  trusted: () => invoke<Trusted[]>("trusted"),
  trust: (fingerprint: string, name: string) => invoke<void>("pair", { fingerprint, name }),
  untrust: (fingerprint: string) => invoke<void>("unpair", { fingerprint }),
  rename: (fingerprint: string, name: string) => invoke<void>("rename_device", { fingerprint, name }),
  setAutoAccept: (fingerprint: string, enabled: boolean) => invoke<void>("set_auto_accept", { fingerprint, enabled }),
  refresh: () => invoke<void>("refresh"),
  respond: (id: string, accept: boolean) => invoke<void>("respond", { id, accept }),
  send: (device: string, paths: string[]) => invoke<string>("send", { device, paths }),
  sendText: (device: string, text: string) => invoke<void>("send_text", { device, text }),
  cancel: (id: string) => invoke<void>("cancel", { id }),
  pause: (id: string) => invoke<void>("pause", { id }),
  resume: (id: string) => invoke<void>("resume", { id }),
  history: () => invoke<HistoryEntry[]>("history"),
  clearHistory: () => invoke<void>("clear_history"),
  startPairing: () => invoke<PairingOffer>("start_pairing"),
  stopPairing: () => invoke<void>("stop_pairing"),
  clipboardSync: () => invoke<boolean>("clipboard_sync"),
  setClipboardSync: (enabled: boolean) => invoke<void>("set_clipboard_sync", { enabled }),
  setDownloads: (path: string) => invoke<void>("set_downloads", { path }),
  openDownloads: () => invoke<void>("open_downloads"),
  reveal: (path: string) => invoke<void>("reveal", { path }),
  openLink: (url: string) => invoke<void>("open_link", { url }),
  autostart: () => invoke<boolean>("autostart"),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
  explorerMenu: () => invoke<boolean>("explorer_menu"),
  setExplorerMenu: (enabled: boolean) => invoke<void>("set_explorer_menu", { enabled }),
  takeLaunchPaths: () => invoke<string[]>("take_launch_paths"),
};

export const events = {
  peers: (f: (p: Peer[]) => void) => listen<Peer[]>("peers", (e) => f(e.payload)),
  incoming: (f: (r: IncomingRequest) => void) => listen<IncomingRequest>("incoming", (e) => f(e.payload)),
  transfer: (f: (u: TransferUpdate) => void) => listen<TransferUpdate>("transfer", (e) => f(e.payload)),
  fatal: (f: (message: string) => void) => listen<string>("fatal", (e) => f(e.payload)),
  paired: (f: (name: string) => void) => listen<string>("paired", (e) => f(e.payload)),
  /** Clipboard text or an image arrived from the named paired device (the content is not sent to the UI). */
  clipboard: (f: (from: string) => void) => listen<string>("clipboard", (e) => f(e.payload)),
  /** A link or text sent on purpose: [text, from]. */
  text: (f: (text: string, from: string) => void) => listen<[string, string]>("text", (e) => f(...e.payload)),
  /** Paths from "Send with MaxxDrop" in Explorer while MaxxDrop was already running. */
  staged: (f: (paths: string[]) => void) => listen<string[]>("staged", (e) => f(e.payload)),
};

export const ACTIVE: TransferState[] = ["WAITING_FOR_ACCEPT", "TRANSFERRING", "VERIFYING"];

export const isLink = (text: string) => /^https?:\/\/[^\s"]+$/.test(text) && text.length < 4096;

export function fromHistory(h: HistoryEntry): Transfer {
  // The files of a paused send are only held while MaxxDrop runs.
  if (h.state === "PAUSED") {
    h = { ...h, state: "CANCELLED", reason: "Paused, then MaxxDrop closed. Send the files again to continue." };
  }
  return {
    id: h.id,
    outgoing: h.outgoing,
    peer: h.peer,
    name: h.name,
    items: h.items,
    total: h.total,
    done: h.state === "COMPLETED" ? h.total : 0,
    state: h.state,
    reason: h.reason,
    saved: h.saved,
    startedAt: h.finished * 1000,
    flowingAt: null,
  };
}
