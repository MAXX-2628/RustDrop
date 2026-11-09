import { useEffect, useRef, useState, type ReactNode } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import {
  CircleAlert,
  Clipboard,
  CircleCheck,
  Clock,
  Copy,
  Download,
  ExternalLink,
  File,
  Files,
  Folder,
  History,
  House,
  Link as LinkIcon,
  LoaderCircle,
  Monitor,
  Pause,
  Pencil,
  Play,
  RefreshCw,
  Send,
  Settings,
  Shield,
  ShieldCheck,
  QrCode as QrIcon,
  Smartphone,
  Trash2,
  X,
  type LucideIcon,
} from "lucide-react";
import logo from "../../shared/brand/logo.svg";
import {
  ACTIVE,
  api,
  events,
  fromHistory,
  isLink,
  type IncomingRequest,
  type PairingOffer,
  type Me,
  type Peer,
  type ReceivedText,
  type Transfer,
  type TransferState,
  type TransferUpdate,
  type Trusted,
} from "./api";
import { savedTheme, setTheme, type ThemePreference } from "./tokens";

type View = "home" | "activity" | "trusted" | "settings";
type Notice = { tone: "error" | "info"; text: string };

const NAV: { id: View; label: string; icon: LucideIcon }[] = [
  { id: "home", label: "Home", icon: House },
  { id: "activity", label: "Activity", icon: History },
  { id: "trusted", label: "Trusted devices", icon: ShieldCheck },
  { id: "settings", label: "Settings", icon: Settings },
];

const REQUEST_TIMEOUT_MS = 60_000; // matches the core's DECISION_TIMEOUT

export function App() {
  const [view, setView] = useState<View>("home");
  const [me, setMe] = useState<Me | null>(null);
  const [fatal, setFatal] = useState<string | null>(null);
  const [peers, setPeers] = useState<Peer[]>([]);
  const [searching, setSearching] = useState(true);
  const [trusted, setTrusted] = useState<Trusted[]>([]);
  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const [requests, setRequests] = useState<IncomingRequest[]>([]);
  const [staged, setStaged] = useState<string[]>([]);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [dragging, setDragging] = useState(false);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [pairName, setPairName] = useState("");
  const [lastPaired, setLastPaired] = useState<{ name: string; at: number } | null>(null);
  const [clipSync, setClipSyncState] = useState(false);
  const [clipFrom, setClipFrom] = useState<string | null>(null);
  /** A link or text waiting for a device, like `staged` for files. */
  const [stagedText, setStagedText] = useState<string | null>(null);
  const [texts, setTexts] = useState<ReceivedText[]>([]);
  const setClipSync = (on: boolean) =>
    api.setClipboardSync(on).then(() => setClipSyncState(on)).catch((e) => setNotice({ tone: "error", text: String(e) }));

  const peersRef = useRef(peers);
  peersRef.current = peers;

  const reloadTrusted = () => api.trusted().then(setTrusted);

  const merge = (u: TransferUpdate) =>
    setTransfers((list) => {
      const clean = Object.fromEntries(Object.entries(u).filter(([, v]) => v !== null)) as Partial<Transfer>;
      const now = Date.now();
      const i = list.findIndex((t) => t.id === u.id);
      if (i === -1) {
        const fresh: Transfer = {
          id: u.id,
          outgoing: false,
          peer: "",
          name: "",
          items: 0,
          total: 0,
          done: 0,
          state: "WAITING_FOR_ACCEPT",
          reason: null,
          saved: null,
          startedAt: now,
          flowingAt: null,
          ...clean,
        };
        return [fresh, ...list].slice(0, 200);
      }
      const next = { ...list[i], ...clean };
      if (next.state === "TRANSFERRING" && next.flowingAt === null) next.flowingAt = now;
      const copy = list.slice();
      copy[i] = next;
      return copy;
    });

  useEffect(() => {
    api.me().then((m) => {
      setMe(m);
      if (m.error) setFatal(m.error);
    });
    api.peers().then(setPeers);
    api.clipboardSync().then(setClipSyncState);
    // Older transfers from history go below anything that already arrived this run.
    api.history().then((h) =>
      setTransfers((list) => [...list, ...h.filter((x) => !list.some((t) => t.id === x.id)).map(fromHistory)]),
    );
    api.takeLaunchPaths().then((paths) => paths.length && setStaged(paths));
    reloadTrusted();
    const stopSearching = setTimeout(() => setSearching(false), 4000);
    const subs = [
      events.peers(setPeers),
      events.transfer((u) => {
        merge(u);
        // The sender cancelled (or it expired): close its accept dialog.
        if (u.state && u.state !== "WAITING_FOR_ACCEPT") setRequests((q) => q.filter((r) => r.id !== u.id));
      }),
      events.fatal(setFatal),
      events.clipboard((from) => {
        setClipFrom(from);
        setTimeout(() => setClipFrom(null), 3000);
      }),
      events.paired((name) => {
        setLastPaired({ name, at: Date.now() });
        reloadTrusted();
      }),
      events.text((text, from) => setTexts((list) => [{ text, from, at: Date.now() }, ...list].slice(0, 5))),
      // "Send with MaxxDrop" in Explorer: each file arrives on its own, so collect them.
      events.staged((paths) => {
        setStaged((list) => [...list, ...paths.filter((p) => !list.includes(p))]);
        setView("home");
      }),
      events.incoming((r) => {
        setRequests((q) => [...q, r]);
        // The core declines unanswered requests after a minute; drop the dialog at the same time.
        setTimeout(() => {
          setRequests((q) => {
            if (q.some((x) => x.id === r.id)) merge({ id: r.id, state: "CANCELLED", reason: "Not answered in time" });
            return q.filter((x) => x.id !== r.id);
          });
        }, REQUEST_TIMEOUT_MS);
      }),
    ];
    return () => {
      clearTimeout(stopSearching);
      subs.forEach((s) => s.then((unlisten) => unlisten()));
    };
  }, []);

  const sendTo = async (peer: Peer, paths: string[]) => {
    if (!peer.trusted) {
      setNotice({ tone: "error", text: `Pair with ${peer.name} before sending.` });
      return;
    }
    try {
      await api.send(peer.id, paths);
      setStaged([]);
      setNotice(null);
    } catch (e) {
      setNotice({ tone: "error", text: String(e) });
    }
  };

  const sendTextTo = async (targets: Peer[], text: string) => {
    const failed: string[] = [];
    for (const p of targets) await api.sendText(p.id, text).catch((e) => failed.push(`${p.name}: ${e}`));
    setStagedText(null);
    setNotice(
      failed.length
        ? { tone: "error", text: failed.join(" ") }
        : { tone: "info", text: `Sent to ${targets.map((p) => p.name).join(", ")}.` },
    );
  };

  /** Sends what is staged (files or text) to every paired device nearby. */
  const sendToAll = async () => {
    const paired = peers.filter((p) => p.trusted);
    if (stagedText) return sendTextTo(paired, stagedText);
    const failed: string[] = [];
    for (const p of paired) await api.send(p.id, staged).catch((e) => failed.push(`${p.name}: ${e}`));
    setStaged([]);
    setNotice(failed.length ? { tone: "error", text: failed.join(" ") } : null);
  };

  const choose = async (directory: boolean) => {
    const picked = await open({ multiple: true, directory });
    if (!picked) return null;
    return Array.isArray(picked) ? picked : [picked];
  };

  const onDevice = async (peer: Peer) => {
    if (peer.trusted === false) {
      setPairName(peer.name);
      setView("trusted");
      return;
    }
    if (stagedText && peer.trusted) return sendTextTo([peer], stagedText);
    if (staged.length) return sendTo(peer, staged);
    const paths = await choose(false);
    if (paths) sendTo(peer, paths);
  };

  const onPick = async (directory: boolean) => {
    const paths = await choose(directory);
    if (paths) {
      setStaged(paths);
      setStagedText(null);
      setNotice(null);
    }
  };

  const deviceAt = (pos: { x: number; y: number }) => {
    const el = document.elementFromPoint(pos.x / devicePixelRatio, pos.y / devicePixelRatio);
    return el?.closest<HTMLElement>("[data-device]")?.dataset.device ?? null;
  };

  // Native drag and drop gives real file paths; handled through a ref so the listener stays current.
  type DropPayload =
    | { type: "enter" | "over"; position: { x: number; y: number } }
    | { type: "drop"; paths: string[]; position: { x: number; y: number } }
    | { type: "leave" };
  const onDrop = useRef<(p: DropPayload) => void>(() => {});
  onDrop.current = (p) => {
    if (p.type === "enter" || p.type === "over") {
      setDragging(true);
      setDropTarget(deviceAt(p.position));
    } else if (p.type === "drop") {
      setDragging(false);
      setDropTarget(null);
      const peer = peersRef.current.find((x) => x.id === deviceAt(p.position));
      if (peer) {
        sendTo(peer, p.paths);
      } else {
        setStaged(p.paths);
        setView("home");
      }
    } else {
      setDragging(false);
      setDropTarget(null);
    }
  };
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((e) => onDrop.current(e.payload as DropPayload));
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  const respond = (r: IncomingRequest, accept: boolean) => {
    setRequests((q) => q.filter((x) => x.id !== r.id));
    api.respond(r.id, accept);
  };

  const fail = (e: unknown) => setNotice({ tone: "error", text: String(e) });
  const transferActions: TransferActions = {
    cancel: (id) => api.cancel(id),
    pause: (id) => api.pause(id),
    resume: (id) => api.resume(id).catch(fail),
    reveal: (path) => api.reveal(path).catch(fail),
  };
  const activeCount = transfers.filter((t) => ACTIVE.includes(t.state)).length;

  return (
    <div className="shell" data-dragging={dragging || undefined}>
      <div className="app" inert={requests.length > 0}>
        <nav className="sidebar" aria-label="Main">
          <div className="brand">
            <img src={logo} alt="" width={28} height={28} />
            <span>MaxxDrop</span>
          </div>
          {NAV.map((n) => (
            <button
              key={n.id}
              className="nav-item"
              aria-label={n.label}
              aria-current={view === n.id ? "page" : undefined}
              onClick={() => setView(n.id)}
            >
              <n.icon size={18} aria-hidden />
              <span className="nav-label">{n.label}</span>
              {n.id === "activity" && activeCount > 0 && (
                <span className="count" aria-hidden>
                  {activeCount}
                </span>
              )}
            </button>
          ))}
        </nav>

        <main className="content">
          {fatal && (
            <p className="banner" role="alert">
              <CircleAlert size={18} aria-hidden />
              MaxxDrop can't receive files right now: {fatal}
            </p>
          )}
          {view === "home" && (
            <HomeView
              clipSync={clipSync}
              clipFrom={clipFrom}
              onPauseClipboard={() => setClipSync(false)}
              me={me}
              peers={peers}
              searching={searching}
              staged={staged}
              dragging={dragging}
              dropTarget={dropTarget}
              notice={notice}
              transfers={transfers}
              stagedText={stagedText}
              texts={texts}
              onDevice={onDevice}
              onPick={onPick}
              onStageText={(t) => {
                setStagedText(t);
                setStaged([]);
                setNotice(null);
              }}
              onClearStaged={() => {
                setStaged([]);
                setStagedText(null);
              }}
              onSendToAll={sendToAll}
              onDismissText={(at) => setTexts((list) => list.filter((x) => x.at !== at))}
              onError={(text) => setNotice({ tone: "error", text })}
              onRefresh={() => api.refresh()}
              actions={transferActions}
              onShowActivity={() => setView("activity")}
            />
          )}
          {view === "activity" && (
            <ActivityView
              transfers={transfers}
              actions={transferActions}
              onClear={() =>
                api
                  .clearHistory()
                  .then(() => setTransfers((list) => list.filter((t) => ACTIVE.includes(t.state) || t.state === "PAUSED")))
                  .catch((e) => setNotice({ tone: "error", text: String(e) }))
              }
            />
          )}
          {view === "trusted" && me && (
            <TrustedView
              me={me}
              trusted={trusted}
              peers={peers}
              pairName={pairName}
              setPairName={setPairName}
              onChanged={reloadTrusted}
              lastPaired={lastPaired}
              onPairedShown={() => setLastPaired(null)}
            />
          )}
          {view === "settings" && me && (
            <SettingsView
              me={me}
              clipSync={clipSync}
              setClipSync={setClipSync}
              onDownloadsChanged={() => api.me().then(setMe)}
            />
          )}
        </main>
      </div>
      {requests[0] && <IncomingDialog key={requests[0].id} request={requests[0]} onAnswer={respond} />}
    </div>
  );
}

type TransferActions = {
  cancel: (id: string) => void;
  pause: (id: string) => void;
  resume: (id: string) => void;
  reveal: (path: string) => void;
};

function HomeView(props: {
  clipSync: boolean;
  clipFrom: string | null;
  onPauseClipboard: () => void;
  me: Me | null;
  peers: Peer[];
  searching: boolean;
  staged: string[];
  stagedText: string | null;
  texts: ReceivedText[];
  dragging: boolean;
  dropTarget: string | null;
  notice: Notice | null;
  transfers: Transfer[];
  onDevice: (p: Peer) => void;
  onPick: (directory: boolean) => void;
  onStageText: (text: string) => void;
  onClearStaged: () => void;
  onSendToAll: () => void;
  onDismissText: (at: number) => void;
  onError: (text: string) => void;
  onRefresh: () => void;
  actions: TransferActions;
  onShowActivity: () => void;
}) {
  const { peers, staged, stagedText } = props;
  const recent = props.transfers.slice(0, 3);
  const pairedCount = peers.filter((p) => p.trusted).length;
  const [composing, setComposing] = useState(false);
  const [draft, setDraft] = useState("");
  const stageDraft = (e: React.FormEvent) => {
    e.preventDefault();
    if (!draft.trim()) return;
    props.onStageText(draft.trim());
    setDraft("");
    setComposing(false);
  };
  return (
    <>
      <header className="page-head">
        <h1>{props.me?.name ?? "This PC"}</h1>
        <p className="status">
          <span className="dot" aria-hidden />
          Ready to receive from paired devices
        </p>
        {props.clipSync && (
          <p className="status" role="status">
            <Clipboard size={16} aria-hidden />
            {props.clipFrom ? `Clipboard updated from ${props.clipFrom}` : "Clipboard sync is on"}
            <button className="btn tertiary" onClick={props.onPauseClipboard}>
              Pause
            </button>
          </p>
        )}
      </header>

      {props.texts.length > 0 && (
        <section aria-label="Received links and text">
          <ul className="texts">
            {props.texts.map((t) => (
              <li key={t.at} className="text-card">
                <LinkIcon size={18} aria-hidden />
                <div className="text-body">
                  <span className="caption">From {t.from}</span>
                  <span className="text-value">{t.text}</span>
                </div>
                {isLink(t.text) && (
                  <button className="btn secondary" onClick={() => api.openLink(t.text).catch((e) => props.onError(String(e)))}>
                    <ExternalLink size={16} aria-hidden />
                    Open
                  </button>
                )}
                <button
                  className="btn tertiary"
                  onClick={() => navigator.clipboard.writeText(t.text).then(() => props.onDismissText(t.at))}
                >
                  <Copy size={16} aria-hidden />
                  Copy
                </button>
                <button className="btn tertiary" aria-label="Dismiss" onClick={() => props.onDismissText(t.at)}>
                  <X size={16} aria-hidden />
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="nearby" aria-labelledby="nearby-title">
        <div className="section-head">
          <h2 id="nearby-title">Nearby devices</h2>
          <button className="btn tertiary" onClick={props.onRefresh}>
            <RefreshCw size={16} aria-hidden />
            Refresh
          </button>
        </div>

        {(staged.length > 0 || stagedText) && (
          <div className="staged" role="status">
            {stagedText ? <LinkIcon size={18} aria-hidden /> : <Files size={18} aria-hidden />}
            <span>
              {stagedText
                ? `"${stagedText.length > 60 ? stagedText.slice(0, 60) + "..." : stagedText}" ready. Choose a device to send it to.`
                : `${plural(staged.length, "item")} ready. Choose a device to send ${staged.length === 1 ? "it" : "them"} to.`}
            </span>
            {pairedCount > 1 && (
              <button className="btn secondary" onClick={props.onSendToAll}>
                Send to all {pairedCount} paired
              </button>
            )}
            <button className="btn tertiary" onClick={props.onClearStaged}>
              Clear
            </button>
          </div>
        )}
        {props.notice && (
          <p className={`notice ${props.notice.tone}`} role="alert">
            <CircleAlert size={16} aria-hidden />
            {props.notice.text}
          </p>
        )}

        {peers.length === 0 ? (
          props.searching ? (
            <Empty icon={<LoaderCircle className="spin" size={28} aria-hidden />} title="Looking for nearby devices..." />
          ) : (
            <Empty
              icon={<Monitor size={28} aria-hidden />}
              title="No devices nearby"
              text="Make sure both devices are connected to the same network and MaxxDrop is running."
              action={
                <button className="btn secondary" onClick={props.onRefresh}>
                  <RefreshCw size={16} aria-hidden />
                  Refresh
                </button>
              }
            />
          )
        ) : (
          <ul className="devices">
            {peers.map((p) => (
              <li key={p.id}>
                <DeviceTile peer={p} target={props.dropTarget === p.id} onClick={() => props.onDevice(p)} />
              </li>
            ))}
          </ul>
        )}
        <p className="hint">
          {props.dragging
            ? "Drop on a paired device to send."
            : "Drag files onto a device, or click a device to choose files."}
        </p>
      </section>

      <div className="actions">
        <button className="btn primary" onClick={() => props.onPick(false)}>
          <Send size={18} aria-hidden />
          Send files
        </button>
        <button className="btn secondary" onClick={() => props.onPick(true)}>
          <Folder size={18} aria-hidden />
          Send a folder
        </button>
        <button className="btn secondary" onClick={() => setComposing(!composing)} aria-expanded={composing}>
          <LinkIcon size={18} aria-hidden />
          Send a link or text
        </button>
      </div>
      {composing && (
        <form className="compose" onSubmit={stageDraft}>
          <label>
            Link or text
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              rows={2}
              maxLength={65536}
              autoFocus
              placeholder="https://..."
            />
          </label>
          <div>
            <button className="btn primary" type="submit" disabled={!draft.trim()}>
              Choose a device
            </button>
          </div>
        </form>
      )}

      {recent.length > 0 && (
        <section aria-labelledby="recent-title">
          <div className="section-head">
            <h2 id="recent-title">Transfers</h2>
            <button className="btn tertiary" onClick={props.onShowActivity}>
              See all
            </button>
          </div>
          <TransferList transfers={recent} actions={props.actions} />
        </section>
      )}
    </>
  );
}

function DeviceTile({ peer, target, onClick }: { peer: Peer; target: boolean; onClick: () => void }) {
  const Icon = peer.platform === "android" ? Smartphone : Monitor;
  const status = peer.trusted === null ? "Checking" : peer.trusted ? "Paired" : "Not paired";
  const action = peer.trusted === false ? `Pair with ${peer.name}` : `Send to ${peer.name}`;
  return (
    <button
      className="device"
      data-device={peer.id}
      data-trusted={peer.trusted === null ? "checking" : String(peer.trusted)}
      data-target={target || undefined}
      onClick={onClick}
      aria-label={`${action}. ${platformName(peer.platform)}, ${status.toLowerCase()}.`}
    >
      <span className="avatar">
        <Icon size={28} aria-hidden />
      </span>
      <span className="device-name">{peer.name}</span>
      <span className="device-meta">{platformName(peer.platform)}</span>
      <span className="trust">
        {peer.trusted === null ? (
          <LoaderCircle className="spin" size={14} aria-hidden />
        ) : peer.trusted ? (
          <ShieldCheck size={14} aria-hidden />
        ) : (
          <Shield size={14} aria-hidden />
        )}
        {target ? "Drop to send" : status}
      </span>
    </button>
  );
}

function ActivityView(props: { transfers: Transfer[]; actions: TransferActions; onClear: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const finished = props.transfers.some((t) => !ACTIVE.includes(t.state) && t.state !== "PAUSED");
  return (
    <>
      <header className="page-head section-head">
        <h1>Activity</h1>
        {finished &&
          (confirming ? (
            <div className="confirm" role="group" aria-label="Clear history?">
              <button
                className="btn destructive"
                onClick={() => {
                  props.onClear();
                  setConfirming(false);
                }}
              >
                Clear history
              </button>
              <button className="btn tertiary" onClick={() => setConfirming(false)}>
                Keep
              </button>
            </div>
          ) : (
            <button className="btn tertiary" onClick={() => setConfirming(true)}>
              <Trash2 size={16} aria-hidden />
              Clear history
            </button>
          ))}
      </header>
      {props.transfers.length === 0 ? (
        <Empty
          icon={<History size={28} aria-hidden />}
          title="Nothing shared yet"
          text="Your transfers will appear here. MaxxDrop keeps the last 200, and never the files themselves."
        />
      ) : (
        <TransferList transfers={props.transfers} actions={props.actions} />
      )}
    </>
  );
}

function TransferList(props: { transfers: Transfer[]; actions: TransferActions }) {
  return (
    <ul className="transfers">
      {props.transfers.map((t) => (
        <li key={t.id}>
          <TransferRow t={t} actions={props.actions} />
        </li>
      ))}
    </ul>
  );
}

const STATE_ICON: Record<TransferState, LucideIcon> = {
  WAITING_FOR_ACCEPT: Clock,
  TRANSFERRING: LoaderCircle,
  VERIFYING: ShieldCheck,
  COMPLETED: CircleCheck,
  FAILED: CircleAlert,
  CANCELLED: X,
  PAUSED: Pause,
};

function TransferRow({ t, actions }: { t: Transfer; actions: TransferActions }) {
  const pct = t.total ? Math.min(100, Math.floor((t.done * 100) / t.total)) : 0;
  const seconds = t.flowingAt ? (Date.now() - t.flowingAt) / 1000 : 0;
  const speed = seconds > 0.5 ? t.done / seconds : 0;
  const left = speed > 0 ? (t.total - t.done) / speed : null;
  const title = t.outgoing ? `To ${t.peer}` : `From ${t.peer}`;
  const what = t.name ? (t.items > 1 ? `${t.name} and ${plural(t.items - 1, "more item")}` : t.name) : plural(t.items, "item");
  const size = t.total ? `${what}, ${formatBytes(t.total)}` : what;
  const when = ACTIVE.includes(t.state) ? "" : new Date(t.startedAt).toLocaleString([], { dateStyle: "medium", timeStyle: "short" });
  const StateIcon = STATE_ICON[t.state];
  const moving = t.state === "TRANSFERRING" || t.state === "VERIFYING";

  let status: string;
  switch (t.state) {
    case "WAITING_FOR_ACCEPT":
      status = t.outgoing ? `Waiting for ${t.peer} to accept` : "Waiting for you to accept";
      break;
    case "TRANSFERRING":
      status = `${t.outgoing ? "Sending" : "Receiving"} ${pct}%`;
      if (speed) status += `, ${formatBytes(speed)}/s`;
      if (left !== null) status += `, ${formatDuration(left)} left`;
      break;
    case "VERIFYING":
      status = "Verifying files";
      break;
    case "COMPLETED":
      status = t.outgoing ? "Sent and verified" : "Received and verified";
      break;
    case "FAILED":
      status = t.reason ? `Transfer failed. ${t.reason}` : "Transfer failed";
      break;
    case "CANCELLED":
      status = t.reason ?? "Cancelled";
      break;
    case "PAUSED":
      status = t.outgoing ? "Paused. Resume continues where it stopped." : (t.reason ?? "Paused");
      break;
  }
  if (when) status += `. ${when}`;

  return (
    <article className="transfer" data-state={t.state}>
      <span className="transfer-icon" aria-hidden>
        {t.outgoing ? <Send size={18} /> : <Download size={18} />}
      </span>
      <div className="transfer-body">
        <div className="transfer-top">
          <h3>{title}</h3>
          <span className="transfer-size">{size}</span>
        </div>
        {moving && (
          <div
            className="bar"
            role="progressbar"
            aria-label={`${title} progress`}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={pct}
          >
            <span style={{ width: `${pct}%` }} />
          </div>
        )}
        <p className="transfer-status">
          <StateIcon size={14} className={t.state === "TRANSFERRING" ? "spin" : undefined} aria-hidden />
          {status}
        </p>
      </div>
      <div className="transfer-actions">
        {t.outgoing && t.state === "TRANSFERRING" && (
          <button className="btn tertiary" onClick={() => actions.pause(t.id)}>
            <Pause size={16} aria-hidden />
            Pause
          </button>
        )}
        {t.outgoing && t.state === "PAUSED" && (
          <button className="btn secondary" onClick={() => actions.resume(t.id)}>
            <Play size={16} aria-hidden />
            Resume
          </button>
        )}
        {((ACTIVE.includes(t.state) && (t.outgoing || t.state !== "WAITING_FOR_ACCEPT")) ||
          (t.outgoing && t.state === "PAUSED")) && (
          <button className="btn tertiary" onClick={() => actions.cancel(t.id)}>
            <X size={16} aria-hidden />
            Cancel
          </button>
        )}
        {!t.outgoing && t.state === "COMPLETED" && t.saved?.[0] && (
          <button className="btn secondary" onClick={() => actions.reveal(t.saved![0])}>
            <Folder size={16} aria-hidden />
            Show in folder
          </button>
        )}
      </div>
    </article>
  );
}

function IncomingDialog({ request, onAnswer }: { request: IncomingRequest; onAnswer: (r: IncomingRequest, accept: boolean) => void }) {
  const decline = useRef<HTMLButtonElement>(null);
  // Focus Decline, not Accept, so a stray Enter never accepts files.
  useEffect(() => decline.current?.focus(), []);
  const Icon = request.platform === "android" ? Smartphone : Monitor;
  const shown = request.files.slice(0, 6);
  return (
    <div className="scrim">
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="incoming-title"
        aria-describedby="incoming-desc"
        onKeyDown={(e) => e.key === "Escape" && onAnswer(request, false)}
      >
        <span className="avatar">
          <Icon size={28} aria-hidden />
        </span>
        <h2 id="incoming-title">
          {request.sender} wants to send you {request.count === 1 ? "a file" : `${request.count} files`}
        </h2>
        <p id="incoming-desc" className="trust-line">
          <ShieldCheck size={16} aria-hidden />
          Paired {platformName(request.platform)} device. {formatBytes(request.total)} in total.
        </p>
        <ul className="file-list">
          {shown.map((f) => (
            <li key={f.path} title={f.path}>
              <File size={16} aria-hidden className="file-icon" />
              <span className="file-name">{f.path}</span>
              <span className="file-size">{formatBytes(f.size)}</span>
            </li>
          ))}
          {request.count > shown.length && <li className="more">and {request.count - shown.length} more</li>}
        </ul>
        <p className="caption">Files are checked before they're saved to your MaxxDrop downloads folder.</p>
        <div className="dialog-actions">
          <button ref={decline} className="btn secondary" onClick={() => onAnswer(request, false)}>
            Decline
          </button>
          <button className="btn primary" onClick={() => onAnswer(request, true)}>
            <Download size={18} aria-hidden />
            Accept
          </button>
        </div>
      </div>
    </div>
  );
}

function TrustedView(props: {
  me: Me;
  trusted: Trusted[];
  peers: Peer[];
  pairName: string;
  setPairName: (name: string) => void;
  onChanged: () => void;
  lastPaired: { name: string; at: number } | null;
  /** Called once the notice is shown, so it does not reappear when this screen opens again. */
  onPairedShown: () => void;
}) {
  const [fingerprint, setFingerprint] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const unpaired = props.peers.filter((p) => p.trusted === false);

  const copy = async () => {
    await navigator.clipboard.writeText(props.me.fingerprint);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const pair = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      await api.trust(fingerprint, props.pairName);
      setFingerprint("");
      props.setPairName("");
      setError(null);
      props.onChanged();
    } catch (err) {
      setError(String(err));
    }
  };

  const remove = async (fp: string) => {
    await api.untrust(fp);
    setConfirming(null);
    props.onChanged();
  };

  const [renaming, setRenaming] = useState<string | null>(null);
  const [newName, setNewName] = useState("");
  const rename = async (e: React.FormEvent, fp: string) => {
    e.preventDefault();
    try {
      await api.rename(fp, newName);
      setRenaming(null);
      props.onChanged();
    } catch (err) {
      setError(String(err));
    }
  };

  const [offer, setOffer] = useState<PairingOffer | null>(null);
  const [secondsLeft, setSecondsLeft] = useState(0);
  const [qrError, setQrError] = useState<string | null>(null);
  const [pairedName, setPairedName] = useState<string | null>(null);

  const showQr = async () => {
    try {
      setOffer(await api.startPairing());
      setSecondsLeft(PAIRING_SECONDS);
      setQrError(null);
      setPairedName(null);
    } catch (e) {
      setQrError(String(e));
    }
  };
  const hideQr = () => {
    setOffer(null);
    api.stopPairing();
  };

  useEffect(() => {
    if (!offer) return;
    if (secondsLeft <= 0) return hideQr();
    const t = setTimeout(() => setSecondsLeft((s) => s - 1), 1000);
    return () => clearTimeout(t);
  }, [offer, secondsLeft]);

  // The other device scanned the code: close it and say who paired.
  useEffect(() => {
    if (!props.lastPaired) return;
    setOffer(null);
    setPairedName(props.lastPaired.name);
    props.onPairedShown();
  }, [props.lastPaired]);

  // Leaving this screen closes an open pairing window.
  useEffect(() => () => void api.stopPairing(), []);

  return (
    <>
      <header className="page-head">
        <h1>Trusted devices</h1>
        <p className="lede">
          Paired devices can ask to send you files. You still accept each transfer.
        </p>
      </header>

      <section className="panel" aria-labelledby="qr-title">
        <h2 id="qr-title">Pair with a QR code</h2>
        {offer ? (
          <div className="qr">
            <QrImage offer={offer} />
            <div className="qr-text">
              <p>
                On the other device, open MaxxDrop, go to Settings and tap <strong>Scan QR code</strong>. Both devices
                will be paired.
              </p>
              <p className="caption" aria-live="polite">
                The code works once and expires in {Math.floor(secondsLeft / 60)}:
                {String(secondsLeft % 60).padStart(2, "0")}.
              </p>
              <button className="btn tertiary" onClick={hideQr}>
                <X size={16} aria-hidden />
                Cancel
              </button>
            </div>
          </div>
        ) : (
          <>
            <p className="caption">Show a code for your phone to scan. Both devices need to be on the same network.</p>
            <button className="btn primary" onClick={showQr}>
              <QrIcon size={18} aria-hidden />
              Show QR code
            </button>
          </>
        )}
        {pairedName && (
          <p className="notice info" role="status">
            <CircleCheck size={16} aria-hidden />
            Paired with {pairedName}.
          </p>
        )}
        {qrError && (
          <p className="notice error" role="alert">
            <CircleAlert size={16} aria-hidden />
            {qrError}
          </p>
        )}
      </section>

      <section className="panel" aria-labelledby="fp-title">
        <h2 id="fp-title">Your fingerprint</h2>
        <p className="caption">Compare it with what the other device shows before you pair.</p>
        <div className="fingerprint">
          <code>{props.me.fingerprint.match(/.{1,4}/g)?.join(" ")}</code>
          <button className="btn tertiary" onClick={copy}>
            <Copy size={16} aria-hidden />
            {copied ? "Copied" : "Copy"}
          </button>
        </div>
      </section>

      <section className="panel" aria-labelledby="pair-title">
        <h2 id="pair-title">Pair with a fingerprint</h2>
        <p className="caption">If scanning isn't possible, enter each other's fingerprint on both devices.</p>
        {unpaired.length > 0 && (
          <div className="chips">
            <span className="caption">Nearby, not paired:</span>
            {unpaired.map((p) => (
              <button key={p.id} className="chip" onClick={() => props.setPairName(p.name)}>
                {p.name}
              </button>
            ))}
          </div>
        )}
        <form className="pair-form" onSubmit={pair}>
          <label>
            Device name
            <input
              value={props.pairName}
              onChange={(e) => props.setPairName(e.target.value)}
              maxLength={64}
              required
            />
          </label>
          <label>
            Their fingerprint
            <textarea
              value={fingerprint}
              onChange={(e) => setFingerprint(e.target.value)}
              rows={2}
              required
              spellCheck={false}
              placeholder="Shown on the other device under Trusted devices"
              aria-invalid={error ? true : undefined}
              aria-describedby={error ? "pair-error" : undefined}
            />
          </label>
          {error && (
            <p id="pair-error" className="notice error" role="alert">
              <CircleAlert size={16} aria-hidden />
              {error}
            </p>
          )}
          <div>
            <button className="btn primary" type="submit">
              <ShieldCheck size={18} aria-hidden />
              Pair device
            </button>
          </div>
        </form>
      </section>

      <section aria-labelledby="paired-title">
        <h2 id="paired-title">Paired</h2>
        {props.trusted.length === 0 ? (
          <Empty icon={<Shield size={28} aria-hidden />} title="No paired devices yet" text="Pair a device above to start sharing." />
        ) : (
          <ul className="trusted-list">
            {props.trusted.map((t) => (
              <li key={t.fingerprint}>
                <span className="avatar small">
                  <ShieldCheck size={18} aria-hidden />
                </span>
                {renaming === t.fingerprint ? (
                  <form className="rename" onSubmit={(e) => rename(e, t.fingerprint)}>
                    <label>
                      <span className="visually-hidden">New name for {t.name}</span>
                      <input value={newName} onChange={(e) => setNewName(e.target.value)} maxLength={64} required autoFocus />
                    </label>
                    <button className="btn primary" type="submit">
                      Save
                    </button>
                    <button className="btn tertiary" type="button" onClick={() => setRenaming(null)}>
                      Cancel
                    </button>
                  </form>
                ) : (
                  <div className="trusted-text">
                    <strong>{t.name}</strong>
                    <code className="caption">{t.fingerprint.slice(0, 19).match(/.{1,4}/g)?.join(" ")}...</code>
                    <label className="switch small">
                      <input
                        type="checkbox"
                        checked={t.auto_accept}
                        onChange={(e) => api.setAutoAccept(t.fingerprint, e.target.checked).then(props.onChanged).catch((err) => setError(String(err)))}
                      />
                      <span className="switch-track" aria-hidden />
                      Accept files from {t.name} without asking
                    </label>
                  </div>
                )}
                {renaming !== t.fingerprint && confirming !== t.fingerprint && (
                  <button
                    className="btn tertiary"
                    aria-label={`Rename ${t.name}`}
                    onClick={() => {
                      setRenaming(t.fingerprint);
                      setNewName(t.name);
                    }}
                  >
                    <Pencil size={16} aria-hidden />
                    Rename
                  </button>
                )}
                {renaming === t.fingerprint ? null : confirming === t.fingerprint ? (
                  <div className="confirm" role="group" aria-label={`Remove ${t.name}?`}>
                    <button className="btn destructive" onClick={() => remove(t.fingerprint)}>
                      Remove {t.name}
                    </button>
                    <button className="btn tertiary" onClick={() => setConfirming(null)}>
                      Keep
                    </button>
                  </div>
                ) : (
                  <button className="btn tertiary" onClick={() => setConfirming(t.fingerprint)}>
                    Remove
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </>
  );
}

const PAIRING_SECONDS = 120; // matches the core's PAIRING_TTL

/** Black on white whatever the theme: scanners need maximum contrast and a light quiet zone. */
function QrImage({ offer }: { offer: PairingOffer }) {
  const quiet = 4;
  const n = offer.size;
  const path = offer.modules
    .map((dark, i) => (dark ? `M${(i % n) + quiet} ${Math.floor(i / n) + quiet}h1v1h-1z` : ""))
    .join("");
  const side = n + quiet * 2;
  return (
    <svg
      className="qr-image"
      viewBox={`0 0 ${side} ${side}`}
      shapeRendering="crispEdges"
      role="img"
      aria-label="Pairing QR code"
    >
      <rect width={side} height={side} fill="#FFFFFF" />
      <path d={path} fill="#000000" />
    </svg>
  );
}

function SettingsView(props: {
  me: Me;
  clipSync: boolean;
  setClipSync: (on: boolean) => void;
  onDownloadsChanged: () => void;
}) {
  const { me, clipSync, setClipSync } = props;
  const [theme, setThemeState] = useState<ThemePreference>(savedTheme());
  const [autostart, setAutostartState] = useState(false);
  const [menu, setMenuState] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.autostart().then(setAutostartState);
    api.explorerMenu().then(setMenuState);
  }, []);
  const choose = (t: ThemePreference) => {
    setThemeState(t);
    setTheme(t);
  };
  const toggle = (set: (on: boolean) => Promise<void>, apply: (on: boolean) => void) => (on: boolean) =>
    set(on)
      .then(() => {
        apply(on);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  const changeFolder = async () => {
    const picked = await open({ directory: true, defaultPath: me.downloads });
    if (typeof picked !== "string") return;
    api
      .setDownloads(picked)
      .then(() => {
        setError(null);
        props.onDownloadsChanged();
      })
      .catch((e) => setError(String(e)));
  };
  return (
    <>
      <header className="page-head">
        <h1>Settings</h1>
      </header>
      {error && (
        <p className="notice error" role="alert">
          <CircleAlert size={16} aria-hidden />
          {error}
        </p>
      )}
      <section className="panel" aria-labelledby="clip-title">
        <h2 id="clip-title">Clipboard</h2>
        <label className="switch">
          <input type="checkbox" checked={clipSync} onChange={(e) => setClipSync(e.target.checked)} />
          <span className="switch-track" aria-hidden />
          Sync clipboard with paired devices
        </label>
        <p className="caption">
          Text and images you copy go to paired devices nearby, and what is copied there comes here, over the
          encrypted connection. MaxxDrop keeps no history of it. Turn it on on each device that should take part.
        </p>
      </section>
      <section className="panel" aria-labelledby="win-title">
        <h2 id="win-title">Windows</h2>
        <label className="switch">
          <input type="checkbox" checked={autostart} onChange={(e) => toggle(api.setAutostart, setAutostartState)(e.target.checked)} />
          <span className="switch-track" aria-hidden />
          Start MaxxDrop when you sign in
        </label>
        <label className="switch">
          <input type="checkbox" checked={menu} onChange={(e) => toggle(api.setExplorerMenu, setMenuState)(e.target.checked)} />
          <span className="switch-track" aria-hidden />
          Show "Send with MaxxDrop" when you right-click files and folders
        </label>
        <p className="caption">
          Closing the window keeps MaxxDrop in the tray, so paired devices can still send to this PC. Quit it from
          the tray icon. On Windows 11, the Explorer item is under "Show more options".
        </p>
      </section>
      <section className="panel" aria-labelledby="theme-title">
        <h2 id="theme-title">Appearance</h2>
        <div className="segmented" role="radiogroup" aria-labelledby="theme-title">
          {(["system", "dark", "light"] as const).map((t) => (
            <label key={t} data-checked={theme === t || undefined}>
              <input type="radio" name="theme" checked={theme === t} onChange={() => choose(t)} />
              {t === "system" ? "Use system setting" : t === "dark" ? "Dark" : "Light"}
            </label>
          ))}
        </div>
      </section>
      <section className="panel" aria-labelledby="dl-title">
        <h2 id="dl-title">Received files</h2>
        <p className="path">{me.downloads}</p>
        <div className="actions">
          <button className="btn secondary" onClick={() => api.openDownloads()}>
            <Folder size={16} aria-hidden />
            Open folder
          </button>
          <button className="btn tertiary" onClick={changeFolder}>
            Change folder
          </button>
        </div>
      </section>
      <section className="panel" aria-labelledby="about-title">
        <h2 id="about-title">This device</h2>
        <dl className="facts">
          <dt>Name</dt>
          <dd>{me.name}</dd>
          <dt>Device ID</dt>
          <dd>
            <code>{me.id}</code>
          </dd>
          <dt>Version</dt>
          <dd>{me.version}</dd>
        </dl>
      </section>
    </>
  );
}

function Empty({ icon, title, text, action }: { icon: ReactNode; title: string; text?: string; action?: ReactNode }) {
  return (
    <div className="empty">
      <span className="avatar">{icon}</span>
      <p className="empty-title">{title}</p>
      {text && <p className="caption">{text}</p>}
      {action}
    </div>
  );
}

function platformName(p: string) {
  return p === "windows" ? "Windows" : p === "android" ? "Android" : p;
}

function plural(n: number, word: string) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function formatBytes(n: number) {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return `${i === 0 ? n : n.toFixed(n < 10 ? 1 : 0)} ${units[i]}`;
}

function formatDuration(s: number) {
  if (s < 60) return `${Math.max(1, Math.ceil(s))} s`;
  if (s < 3600) return `${Math.ceil(s / 60)} min`;
  return `${Math.floor(s / 3600)} h ${Math.ceil((s % 3600) / 60)} min`;
}
