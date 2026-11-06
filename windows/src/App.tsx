import { useEffect, useRef, useState, type ReactNode } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import {
  CircleAlert,
  CircleCheck,
  Clock,
  Copy,
  Download,
  Files,
  Folder,
  History,
  House,
  LoaderCircle,
  Monitor,
  RefreshCw,
  Send,
  Settings,
  Shield,
  ShieldCheck,
  Smartphone,
  X,
  type LucideIcon,
} from "lucide-react";
import logo from "../../shared/brand/logo.svg";
import {
  ACTIVE,
  api,
  events,
  type IncomingRequest,
  type Me,
  type Peer,
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
    reloadTrusted();
    const stopSearching = setTimeout(() => setSearching(false), 4000);
    const subs = [
      events.peers(setPeers),
      events.transfer(merge),
      events.fatal(setFatal),
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
    if (staged.length) return sendTo(peer, staged);
    const paths = await choose(false);
    if (paths) sendTo(peer, paths);
  };

  const onPick = async (directory: boolean) => {
    const paths = await choose(directory);
    if (paths) {
      setStaged(paths);
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

  const reveal = (path: string) => api.reveal(path).catch((e) => setNotice({ tone: "error", text: String(e) }));
  const activeCount = transfers.filter((t) => ACTIVE.includes(t.state)).length;

  return (
    <div className="shell" data-dragging={dragging || undefined}>
      <div className="app" inert={requests.length > 0}>
        <nav className="sidebar" aria-label="Main">
          <div className="brand">
            <img src={logo} alt="" width={28} height={28} />
            <span>RustDrop</span>
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
              RustDrop can't receive files right now: {fatal}
            </p>
          )}
          {view === "home" && (
            <HomeView
              me={me}
              peers={peers}
              searching={searching}
              staged={staged}
              dragging={dragging}
              dropTarget={dropTarget}
              notice={notice}
              transfers={transfers}
              onDevice={onDevice}
              onPick={onPick}
              onClearStaged={() => setStaged([])}
              onRefresh={() => api.refresh()}
              onCancel={(id) => api.cancel(id)}
              onReveal={reveal}
              onShowActivity={() => setView("activity")}
            />
          )}
          {view === "activity" && (
            <ActivityView transfers={transfers} onCancel={(id) => api.cancel(id)} onReveal={reveal} />
          )}
          {view === "trusted" && me && (
            <TrustedView
              me={me}
              trusted={trusted}
              peers={peers}
              pairName={pairName}
              setPairName={setPairName}
              onChanged={reloadTrusted}
            />
          )}
          {view === "settings" && me && <SettingsView me={me} />}
        </main>
      </div>
      {requests[0] && <IncomingDialog key={requests[0].id} request={requests[0]} onAnswer={respond} />}
    </div>
  );
}

function HomeView(props: {
  me: Me | null;
  peers: Peer[];
  searching: boolean;
  staged: string[];
  dragging: boolean;
  dropTarget: string | null;
  notice: Notice | null;
  transfers: Transfer[];
  onDevice: (p: Peer) => void;
  onPick: (directory: boolean) => void;
  onClearStaged: () => void;
  onRefresh: () => void;
  onCancel: (id: string) => void;
  onReveal: (path: string) => void;
  onShowActivity: () => void;
}) {
  const { peers, staged } = props;
  const recent = props.transfers.slice(0, 3);
  return (
    <>
      <header className="page-head">
        <h1>{props.me?.name ?? "This PC"}</h1>
        <p className="status">
          <span className="dot" aria-hidden />
          Ready to receive from paired devices
        </p>
      </header>

      <section className="nearby" aria-labelledby="nearby-title">
        <div className="section-head">
          <h2 id="nearby-title">Nearby devices</h2>
          <button className="btn tertiary" onClick={props.onRefresh}>
            <RefreshCw size={16} aria-hidden />
            Refresh
          </button>
        </div>

        {staged.length > 0 && (
          <div className="staged" role="status">
            <Files size={18} aria-hidden />
            <span>
              {plural(staged.length, "item")} ready. Choose a device to send {staged.length === 1 ? "it" : "them"} to.
            </span>
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
              text="Make sure both devices are connected to the same network and RustDrop is running."
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
      </div>

      {recent.length > 0 && (
        <section aria-labelledby="recent-title">
          <div className="section-head">
            <h2 id="recent-title">Transfers</h2>
            <button className="btn tertiary" onClick={props.onShowActivity}>
              See all
            </button>
          </div>
          <TransferList transfers={recent} onCancel={props.onCancel} onReveal={props.onReveal} />
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

function ActivityView(props: { transfers: Transfer[]; onCancel: (id: string) => void; onReveal: (p: string) => void }) {
  return (
    <>
      <header className="page-head">
        <h1>Activity</h1>
      </header>
      {props.transfers.length === 0 ? (
        <Empty
          icon={<History size={28} aria-hidden />}
          title="Nothing shared yet"
          text="Your recent transfers will appear here."
        />
      ) : (
        <TransferList {...props} />
      )}
    </>
  );
}

function TransferList(props: { transfers: Transfer[]; onCancel: (id: string) => void; onReveal: (p: string) => void }) {
  return (
    <ul className="transfers">
      {props.transfers.map((t) => (
        <li key={t.id}>
          <TransferRow t={t} onCancel={props.onCancel} onReveal={props.onReveal} />
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
};

function TransferRow({ t, onCancel, onReveal }: { t: Transfer; onCancel: (id: string) => void; onReveal: (p: string) => void }) {
  const pct = t.total ? Math.min(100, Math.floor((t.done * 100) / t.total)) : 0;
  const seconds = t.flowingAt ? (Date.now() - t.flowingAt) / 1000 : 0;
  const speed = seconds > 0.5 ? t.done / seconds : 0;
  const left = speed > 0 ? (t.total - t.done) / speed : null;
  const title = t.outgoing ? `To ${t.peer}` : `From ${t.peer}`;
  const size = t.total ? `${plural(t.items, "item")}, ${formatBytes(t.total)}` : plural(t.items, "item");
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
  }

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
        {t.outgoing && ACTIVE.includes(t.state) && (
          <button className="btn tertiary" onClick={() => onCancel(t.id)}>
            <X size={16} aria-hidden />
            Cancel
          </button>
        )}
        {!t.outgoing && t.state === "COMPLETED" && t.saved?.[0] && (
          <button className="btn secondary" onClick={() => onReveal(t.saved![0])}>
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
            <li key={f.path}>
              <span className="file-name">{f.path}</span>
              <span className="file-size">{formatBytes(f.size)}</span>
            </li>
          ))}
          {request.count > shown.length && <li className="more">and {request.count - shown.length} more</li>}
        </ul>
        <p className="caption">Files are checked before they're saved to your RustDrop downloads folder.</p>
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

  return (
    <>
      <header className="page-head">
        <h1>Trusted devices</h1>
        <p className="lede">
          Paired devices can ask to send you files. You still accept each transfer. Pair on both devices, using each
          other's fingerprint.
        </p>
      </header>

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
        <h2 id="pair-title">Pair a device</h2>
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
                <div className="trusted-text">
                  <strong>{t.name}</strong>
                  <code className="caption">{t.fingerprint.slice(0, 19).match(/.{1,4}/g)?.join(" ")}...</code>
                </div>
                {confirming === t.fingerprint ? (
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

function SettingsView({ me }: { me: Me }) {
  const [theme, setThemeState] = useState<ThemePreference>(savedTheme());
  const choose = (t: ThemePreference) => {
    setThemeState(t);
    setTheme(t);
  };
  return (
    <>
      <header className="page-head">
        <h1>Settings</h1>
      </header>
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
        <button className="btn secondary" onClick={() => api.openDownloads()}>
          <Folder size={16} aria-hidden />
          Open folder
        </button>
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
