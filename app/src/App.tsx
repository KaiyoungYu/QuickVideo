import { FormEvent, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ask, open, save } from "@tauri-apps/plugin-dialog";
import appIcon from "../src-tauri/icons/128x128.png";
import "./App.css";

type TaskState = "queued" | "preparing" | "transcribing" | "summarizing" | "cancelling" | "completed" | "failed" | "interrupted" | "orphaned" | "cancelled";
type Screen = "library" | "setup" | "settings";

type VideoTask = {
  id: number;
  title: string;
  source: string;
  duration: string;
  state: TaskState;
  progress: number;
  createdAt: string;
  inputRef: string;
  subtitleSource?: string | null;
  errorMessage?: string | null;
};

type PlatformSubtitles = { transcript: string; source: string };
type MediaMetadata = { title: string; duration: string };
type SelectedLocalFile = { path: string; name: string };
type ApiCall = { id: number; createdAt: string; purpose: string; provider: string; baseUrl: string; model: string; success: boolean; statusCode?: number | null; durationMs: number; errorMessage?: string | null; traceId?: string | null; requestHeaders?: string | null; requestBody?: string | null; responseHeaders?: string | null; responseBody?: string | null };
type ExportType = "summaryAndTranscript" | "summary" | "transcript";

const stateCopy: Record<TaskState, string> = {
  queued: "等待处理",
  preparing: "正在准备",
  transcribing: "正在转写",
  summarizing: "正在提炼章节",
  cancelling: "正在终止",
  completed: "已生成笔记",
  failed: "处理失败",
  interrupted: "已中断",
  orphaned: "遗留进程",
  cancelled: "已终止",
};

const activeStates: TaskState[] = ["queued", "preparing", "transcribing", "summarizing", "cancelling"];
const restartableStates: TaskState[] = ["failed", "interrupted", "cancelled"];
const isCancellationError = (error: unknown) => String(error).includes("任务已由用户终止");

function formatTaskTime(value: string): string {
  const instant = Date.parse(value);
  if (Number.isNaN(instant)) return value;
  const elapsed = Math.max(0, Date.now() - instant);
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 1) return "刚刚完成";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days} 天前`;
  const date = new Date(instant);
  return `${date.getFullYear()}年${date.getMonth() + 1}月${date.getDate()}日`;
}

function NavIcon({ name }: { name: "library" | "model" | "settings" }) {
  if (name === "library") return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 5.5h14M5 12h14M5 18.5h9" /></svg>;
  if (name === "model") return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3v4M12 17v4M4.2 7.5l3.4 2M16.4 14.5l3.4 2M4.2 16.5l3.4-2M16.4 9.5l3.4-2" /><circle cx="12" cy="12" r="3.5" /></svg>;
  return <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 3.5v2M12 18.5v2M3.5 12h2M18.5 12h2M6 6l1.4 1.4M16.6 16.6L18 18M18 6l-1.4 1.4M7.4 16.6L6 18" /><circle cx="12" cy="12" r="4" /></svg>;
}

function App() {
  const [tasks, setTasks] = useState<VideoTask[]>([]);
  const [source, setSource] = useState("");
  const [linkConsent, setLinkConsent] = useState(false);
  const [pendingLocalFile, setPendingLocalFile] = useState<SelectedLocalFile | null>(null);
  const [notice, setNotice] = useState("本地优先处理，临时媒体将在完成后自动清理。");
  const [activeTask, setActiveTask] = useState<VideoTask | null>(null);
  const [screen, setScreen] = useState<Screen>("library");
  const cancellingTaskIds = useRef(new Set<number>());

  useEffect(() => {
    invoke<number>("recover_incomplete_tasks")
      .then((count) => {
        if (count > 0) setNotice(`${count} 个任务在应用关闭后中断，可点击“重新开始”。`);
        return invoke<VideoTask[]>("list_tasks");
      })
      .then(setTasks)
      .catch(() => setNotice("本地资料库暂不可用，请重新启动桌面 App。"));
    invoke<{ ffmpeg: boolean; whisper: boolean; model: boolean; ytdlp: boolean }>("check_runtime").then((runtime) => { if (!runtime.ffmpeg || !runtime.whisper || !runtime.model) setNotice("本地转写组件尚未完整安装，请在设置中完成模型与工具检查。 "); }).catch(() => undefined);
  }, []);
  const updateTask = (id: number, changes: Partial<VideoTask>) => {
    setTasks((current) =>
      current.map((task) => (task.id === id ? { ...task, ...changes } : task)),
    );
    if (changes.state || changes.progress !== undefined || changes.title || changes.duration) {
      void invoke("update_task", { taskId: id, state: changes.state ?? "queued", progress: changes.progress ?? 0, title: changes.title ?? null, duration: changes.duration ?? null, subtitleSource: changes.subtitleSource ?? null, errorMessage: changes.errorMessage ?? null }).catch(() => undefined);
    }
  };

  const addTask = async (label: string, taskSource: string, inputRef: string): Promise<VideoTask> => {
    const saved = await invoke<VideoTask>("create_task", { title: label, source: taskSource, inputRef });
    setTasks((current) => [saved, ...current]);
    return saved;
  };

  const updateTaskMetadata = async (id: number, changes: Pick<Partial<VideoTask>, "title" | "duration">) => {
    setTasks((current) => current.map((task) => task.id === id ? { ...task, ...changes } : task));
    await invoke("update_task_metadata", { taskId: id, title: changes.title ?? null, duration: changes.duration ?? null });
  };

  const finishTranscript = async (task: VideoTask, transcript: string, subtitleSource: string) => {
    updateTask(task.id, { state: "summarizing", progress: 76 });
    let summary = "## 待生成摘要\n\n字幕已保存。请在“模型”页配置 API Key，再点击“重新生成摘要”。";
    try {
      summary = await invoke<string>("generate_summary", { transcript });
    } catch {
      // 字幕仍应可离线保存；模型配置是可选的第二步。
    }
    await invoke("save_note", { taskId: task.id, transcript, summary });
    updateTask(task.id, { state: "completed", progress: 100, createdAt: "刚刚完成", subtitleSource });
  };

  const processLink = async (task: VideoTask, url: string) => {
    await invoke("activate_task", { taskId: task.id });
    updateTask(task.id, { state: "preparing", progress: 6, errorMessage: null });
    try {
      const metadata = await invoke<MediaMetadata>("get_link_metadata", { url });
      await updateTaskMetadata(task.id, { title: metadata.title, duration: metadata.duration });
    } catch {
      // 元数据失败不阻止字幕与转写主流程。
    }
    updateTask(task.id, { state: "transcribing", progress: 18, errorMessage: null });
    try {
      let srt: string;
      let subtitleSource = "平台字幕";
      try {
        const platformSubtitles = await invoke<PlatformSubtitles>("fetch_platform_subtitles", { url, taskId: task.id });
        srt = platformSubtitles.transcript;
        subtitleSource = platformSubtitles.source;
      } catch {
        setNotice("平台没有可用字幕，正在临时提取音频并使用本机 whisper.cpp 转写…");
        updateTask(task.id, { state: "transcribing", progress: 38 });
        srt = await invoke<string>("transcribe_platform_link", { url, taskId: task.id });
        subtitleSource = "本地 whisper.cpp（临时音频）";
      }
      await finishTranscript(task, srt, subtitleSource);
      setNotice(`已保存${subtitleSource}与笔记（${srt.split("\n\n").length} 段）。`);
    } catch (error) {
      if (cancellingTaskIds.current.has(task.id) || isCancellationError(error)) {
        updateTask(task.id, { state: "cancelled", progress: 0, errorMessage: null });
        setNotice("任务已终止，相关临时资源将被清理。");
      } else {
        updateTask(task.id, { state: "failed", progress: 0, errorMessage: String(error) });
        setNotice(`无法完成链接分析：${String(error)}。可稍后重新开始，或导入已获授权的本地文件。`);
      }
    } finally {
      cancellingTaskIds.current.delete(task.id);
      void invoke("release_task", { taskId: task.id });
    }
  };

  const processLocalFile = async (task: VideoTask, inputPath: string) => {
    await invoke("activate_task", { taskId: task.id });
    updateTask(task.id, { state: "preparing", progress: 6, errorMessage: null });
    try {
      const duration = await invoke<string>("get_local_duration", { inputPath });
      await updateTaskMetadata(task.id, { duration });
    } catch {
      // 个别容器无法读取时长时，仍允许继续转写。
    }
    updateTask(task.id, { state: "transcribing", progress: 12, errorMessage: null });
    try {
      const srt = await invoke<string>("transcribe_local_file", { inputPath, taskId: task.id });
      await finishTranscript(task, srt, "本地 whisper.cpp");
      setNotice(`本地转写完成：已保存 ${srt.split("\n\n").length} 段字幕与笔记。`);
    } catch (error) {
      if (cancellingTaskIds.current.has(task.id) || isCancellationError(error)) {
        updateTask(task.id, { state: "cancelled", progress: 0, errorMessage: null });
        setNotice("任务已终止，相关临时资源将被清理。");
      } else {
        updateTask(task.id, { state: "failed", progress: 0, errorMessage: String(error) });
        setNotice(`本地转写失败：${String(error)}`);
      }
    } finally {
      cancellingTaskIds.current.delete(task.id);
      void invoke("release_task", { taskId: task.id });
    }
  };

  const retryTask = (task: VideoTask) => {
    if (!task.inputRef) { setNotice("这个早期任务没有保存输入来源，请重新添加一次。 "); return; }
    setNotice("正在重新处理任务…");
    updateTask(task.id, { state: "queued", progress: 0, errorMessage: null });
    if (task.source.includes("链接")) void processLink(task, task.inputRef);
    else void processLocalFile(task, task.inputRef);
  };

  const terminateTask = async (task: VideoTask) => {
    cancellingTaskIds.current.add(task.id);
    updateTask(task.id, { state: "cancelling", progress: task.progress, errorMessage: null });
    setNotice("正在终止任务并释放相关资源…");
    try {
      const signalled = await invoke<boolean>("cancel_task", { taskId: task.id });
      if (!signalled) {
        updateTask(task.id, { state: "cancelled", progress: 0, errorMessage: null });
        cancellingTaskIds.current.delete(task.id);
        void invoke("release_task", { taskId: task.id });
        setNotice("任务已终止。未发现仍在运行的本地子进程。");
      }
    } catch (error) {
      cancellingTaskIds.current.delete(task.id);
      updateTask(task.id, { state: "failed", progress: 0, errorMessage: String(error) });
      setNotice(`无法终止任务：${String(error)}`);
    }
  };

  const submitLink = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (isProcessing) { setNotice("当前正在处理一段视频。首版一次只处理一个任务，请等待它完成或失败后再继续。 "); return; }
    if (!linkConsent) { setNotice("请先确认你拥有或已获授权处理该链接内容。 "); return; }
    const trimmed = source.trim();
    if (!trimmed) {
      setNotice("请先粘贴 YouTube 或 Bilibili 的视频链接。");
      return;
    }

    const isBilibili = trimmed.includes("bilibili.com");
    const isYouTube = trimmed.includes("youtube.com") || trimmed.includes("youtu.be");
    if (!isBilibili && !isYouTube) {
      setNotice("目前只支持 YouTube、Bilibili 或本地媒体文件。");
      return;
    }
    const platform = isBilibili ? "Bilibili 链接" : "YouTube 链接";
    void (async () => {
      const task = await addTask("正在获取平台字幕", platform, trimmed);
      await processLink(task, trimmed);
    })().catch((error) => setNotice(`创建任务失败：${String(error)}`));
    setSource("");
  };

  const chooseLocalFile = async () => {
    if (isProcessing) { setNotice("当前正在处理一段视频。首版一次只处理一个任务，请等待它完成或失败后再继续。 "); return; }
    const selected = await open({ multiple: false, directory: false, filters: [{ name: "音视频", extensions: ["mp4", "mov", "mkv", "mp3", "m4a", "wav"] }] });
    if (!selected || Array.isArray(selected)) return;
    const rawName = selected.split("/").pop()?.replace(/\.[^.]+$/, "") ?? "本地媒体";
    const name = rawName.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 72) || "本地媒体";
    setPendingLocalFile({ path: selected, name });
    setNotice("文件已选择。确认后点击“开始处理”，再开始本机转写。 ");
  };

  const startLocalFile = () => {
    if (!pendingLocalFile || isProcessing) return;
    const selected = pendingLocalFile;
    setPendingLocalFile(null);
    setNotice("正在使用本机 whisper.cpp 转写；长视频可能需要一些时间。 ");
    void (async () => {
      const task = await addTask(selected.name, "本地文件 · whisper.cpp", selected.path);
      await processLocalFile(task, selected.path);
    })().catch((error) => setNotice(`创建任务失败：${String(error)}`));
  };

  const completedCount = tasks.filter((task) => task.state === "completed").length;
  const isProcessing = tasks.some((task) => activeStates.includes(task.state));

  if (activeTask) {
    return <NoteDetail task={activeTask} onBack={() => setActiveTask(null)} onOpenSetup={() => { setActiveTask(null); setScreen("setup"); }} />;
  }

  if (screen === "setup") {
    return <ProviderSetup onBack={() => setScreen("library")} onSaved={() => { setNotice("模型服务已保存到本机。现在可以开始生成真实摘要。 "); setScreen("library"); }} />;
  }

  if (screen === "settings") {
    return <SettingsPage onBack={() => setScreen("library")} onEditModel={() => setScreen("setup")} onClearLibrary={() => { void invoke("clear_tasks").catch(() => undefined); setTasks([]); setNotice("资料库已清空。可以从一段新的视频开始。 "); setScreen("library"); }} />;
  }

  return (
    <main className="app-shell">
      <aside className="rail" aria-label="主导航">
        <div className="brand-mark" aria-label="快看视频 QuickVideo">
          <img src={appIcon} alt="" />
          <i />
        </div>
        <nav>
          <button className="nav-item active" type="button" onClick={() => setScreen("library")} aria-label="资料库">
            <span className="nav-icon"><NavIcon name="library" /></span>
            <span>资料库</span>
          </button>
          <button className="nav-item" type="button" onClick={() => setScreen("setup")} aria-label="模型设置">
            <span className="nav-icon"><NavIcon name="model" /></span>
            <span>模型</span>
          </button>
          <button className="nav-item" type="button" onClick={() => setScreen("settings")} aria-label="设置">
            <span className="nav-icon"><NavIcon name="settings" /></span>
            <span>设置</span>
          </button>
        </nav>
        <div className="rail-foot">
          <span className="local-pill"><b /> LOCAL</span>
          <small>v0.1</small>
        </div>
      </aside>

      <section className="workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">PERSONAL VIDEO READER</p>
            <h1>把视频，变成<br /><em>可以回看的知识。</em></h1>
          </div>
          <div className="today-card">
            <span>本周已整理</span>
            <strong>{completedCount.toString().padStart(2, "0")}</strong>
            <small>段视频内容</small>
          </div>
        </header>

        <section className="ingest-panel" aria-labelledby="ingest-title">
          <div className="panel-heading">
            <div>
              <p className="step-label">01 — INPUT</p>
              <h2 id="ingest-title">开始一段新的理解</h2>
            </div>
            <span className="privacy-note">你的内容，先留在你的 Mac</span>
          </div>

          <form className="link-form" onSubmit={submitLink}>
            <label className="source-field">
              <span>粘贴 YouTube 或 Bilibili 链接</span>
              <input
                value={source}
                onChange={(event) => setSource(event.target.value)}
                placeholder="https://..."
                inputMode="url"
                disabled={isProcessing}
              />
            </label>
            <button className="primary-action" type="submit" disabled={isProcessing || !linkConsent}>
              分析链接 <span>↗</span>
            </button>
          </form>
          <label className="consent-line"><input type="checkbox" checked={linkConsent} onChange={(event) => setLinkConsent(event.target.checked)} /><span>我确认拥有或已获授权处理该内容；不请求绕过登录、付费、DRM 或其他访问限制。</span></label>

          <div className="ingest-divider"><span>或</span></div>

          <button className="file-drop" type="button" onClick={() => void chooseLocalFile()} disabled={isProcessing}>
            <span className="file-icon">↓</span>
            <span><b>导入本地视频或音频</b><small>MP4、MOV、MP3、M4A · 优先推荐</small></span>
            <i>选择文件</i>
          </button>
          {pendingLocalFile && <div className="selected-file" role="status">
            <div><span>已选择本地文件</span><b>{pendingLocalFile.name}</b></div>
            <button type="button" onClick={() => setPendingLocalFile(null)}>取消</button>
            <button className="start-local" type="button" onClick={startLocalFile}>开始处理 ↗</button>
          </div>}
          {isProcessing && <div className="processing-banner" role="status" aria-live="polite">
            <i />
            <div><b>{tasks.some((task) => task.state === "summarizing") ? "正在生成结构化笔记" : "正在本机转写"}</b><small>任务正在后台运行；你可以继续浏览资料库，请保持 App 打开。</small></div>
            <span>处理中</span>
          </div>}
          <p className="inline-notice">{notice}</p>
        </section>

        <section className="library" aria-labelledby="library-title">
          <div className="section-heading">
            <div>
              <p className="step-label">02 — LIBRARY</p>
              <h2 id="library-title">正在阅读</h2>
            </div>
            <button className="text-button" type="button">查看全部 <span>→</span></button>
          </div>

          <div className="task-list">
            {tasks.length === 0 ? (
              <div className="empty-library"><span>◎</span><h3>还没有任何视频笔记</h3><p>从一条链接或一个本地文件开始。你会在这里看到每一次理解的积累。</p></div>
            ) : tasks.map((task, index) => (
              <article className={`task-card ${task.state}`} key={task.id}>
                <div className="task-index">{String(index + 1).padStart(2, "0")}</div>
                <div className="task-main">
                  <div className="task-title-row">
                    <h3>{task.title}</h3>
                    <span className="status"><i />{stateCopy[task.state]}</span>
                  </div>
                  <p>{task.source} <b>·</b> {task.duration}</p>
                  {!(["completed", "failed", "interrupted", "cancelled", "orphaned"] as TaskState[]).includes(task.state) && (
                    <div className={`progress ${["preparing", "transcribing", "summarizing", "cancelling"].includes(task.state) ? "running" : ""}`} aria-label={`${task.progress}% 完成`}>
                      <span style={{ width: `${task.progress}%` }} />
                    </div>
                  )}
                </div>
                <div className="task-time" title={task.createdAt}>{formatTaskTime(task.createdAt)}</div>
                {restartableStates.includes(task.state) && <button className="task-retry" type="button" onClick={() => retryTask(task)}>重新开始</button>}
                {["preparing", "transcribing", "summarizing", "orphaned"].includes(task.state) && <button className="task-cancel" type="button" onClick={() => void terminateTask(task)}>终止</button>}
                <button
                  className="task-open"
                  type="button"
                  disabled={task.state !== "completed"}
                  onClick={() => task.state === "completed" && setActiveTask(task)}
                  aria-label={task.state === "completed" ? `打开 ${task.title}` : `${task.title} 尚未完成`}
                >↗</button>
              </article>
            ))}
          </div>
        </section>
      </section>
    </main>
  );
}

function ProviderSetup({ onBack, onSaved }: { onBack: () => void; onSaved: () => void }) {
  const [model, setModel] = useState("Pro/zai-org/GLM-4.7");
  const [summaryPrompt, setSummaryPrompt] = useState("");
  const [key, setKey] = useState("");
  const [keyConfigured, setKeyConfigured] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState("");
  const [testing, setTesting] = useState(false);

  useEffect(() => { invoke<{ model: string; summary_prompt: string; configured: boolean }>("get_model_config").then((config) => { setModel(config.model); setSummaryPrompt(config.summary_prompt); setKeyConfigured(config.configured); }).catch(() => undefined); }, []);
  const saveProvider = async () => {
    if (!key.trim() && !keyConfigured) { setSaveError("请先填写 API Key。它将以明文保存在本机应用配置中。 "); return; }
    setSaveError("");
    setTesting(true);
    try {
      const savedConfig = await invoke<{ configured: boolean }>("save_model_config", { model, summaryPrompt, apiKey: key });
      if (!savedConfig.configured) throw new Error("API Key 未能写入本机配置");
      setKeyConfigured(savedConfig.configured);
      setKey("");
      await invoke("test_model_connection");
      setSaved(true);
      window.setTimeout(onSaved, 800);
    } catch (error) {
      setSaved(false);
      setSaveError(`连接测试失败：${String(error)}。配置已保存，可修改地址或模型名称后再次测试。`);
    } finally {
      setTesting(false);
    }
  };

  return (
    <main className="config-page">
      <header className="config-topbar"><button type="button" onClick={onBack}>← 返回资料库</button><span>QUICKVIDEO · 初始设置</span></header>
      <section className="config-card setup-card">
        <p className="eyebrow">ONE SMALL SETUP</p>
        <h1>让笔记，<em>用你的模型</em>生成。</h1>
        <p className="config-intro">第一版仅接入 SiliconFlow。字幕只会发送到其固定 API 端点生成摘要；费用由你的 SiliconFlow 账户承担。</p>
        <div className="siliconflow-endpoint"><span>SILICONFLOW API</span><b>https://api.siliconflow.cn/v1/chat/completions</b></div>
        <div className="config-grid">
          <label className="config-field"><span>模型名称</span><input value={model} onChange={(event) => setModel(event.target.value)} /></label>
          <label className="config-field"><span>API Key {keyConfigured && <em>已保存在本机</em>}</span><input value={key} type="password" onChange={(event) => setKey(event.target.value)} placeholder={keyConfigured ? "留空则保持当前 Key 不变" : "请输入 API Key"} /></label>
        </div>
        <label className="config-field prompt-field"><span>摘要 Prompt <em>可编辑</em></span><textarea value={summaryPrompt} onChange={(event) => setSummaryPrompt(event.target.value)} spellCheck={false} placeholder="正在加载默认模板…" /></label>
        <p className="prompt-hint">它决定最终笔记的结构与表达。长视频仍会先分段提炼，再按这里的要求合并。</p>
        <div className="key-safe"><b>!</b><span>不会上传至我们的服务器，但会以明文保存在本机应用配置中。Key 已保存时，保持输入框为空即可修改其他设置。</span></div>
        {saveError && <p className="provider-error">{saveError}</p>}
        <button className="save-provider" type="button" disabled={testing} onClick={saveProvider}>{testing ? "正在测试连接…" : saved ? "已保存且连接成功 ✓" : "保存并测试连接 →"}</button>
      </section>
    </main>
  );
}

function SettingsPage({ onBack, onEditModel, onClearLibrary }: { onBack: () => void; onEditModel: () => void; onClearLibrary: () => void }) {
  const [cleanup, setCleanup] = useState(true);
  const [language, setLanguage] = useState("自动识别（中文 / English）");
  const [summaryTimeoutManual, setSummaryTimeoutManual] = useState(false);
  const [summaryTimeoutMinutes, setSummaryTimeoutMinutes] = useState(10);
  const [saved, setSaved] = useState("");
  const [runtime, setRuntime] = useState<{ ffmpeg: boolean; whisper: boolean; model: boolean; ytdlp: boolean } | null>(null);
  const [downloadingModel, setDownloadingModel] = useState(false);
  const [apiCalls, setApiCalls] = useState<ApiCall[]>([]);
  const [callsLoading, setCallsLoading] = useState(false);
  const refreshRuntime = () => { void invoke<{ ffmpeg: boolean; whisper: boolean; model: boolean; ytdlp: boolean }>("check_runtime").then(setRuntime).catch(() => setRuntime(null)); };
  const refreshCalls = () => { setCallsLoading(true); void invoke<ApiCall[]>("list_api_calls").then(setApiCalls).catch(() => setApiCalls([])).finally(() => setCallsLoading(false)); };
  useEffect(() => { invoke<{ cleanupTempMedia: boolean; transcriptionLanguage: string; summaryTimeoutManual: boolean; summaryTimeoutMinutes: number }>("get_preferences").then((value) => { setCleanup(value.cleanupTempMedia); setLanguage(value.transcriptionLanguage); setSummaryTimeoutManual(value.summaryTimeoutManual); setSummaryTimeoutMinutes(value.summaryTimeoutMinutes); }).catch(() => undefined); refreshRuntime(); refreshCalls(); }, []);
  const persist = (nextCleanup: boolean, nextLanguage: string, nextSummaryTimeoutManual = summaryTimeoutManual, nextSummaryTimeoutMinutes = summaryTimeoutMinutes) => {
    setCleanup(nextCleanup);
    setLanguage(nextLanguage);
    setSummaryTimeoutManual(nextSummaryTimeoutManual);
    setSummaryTimeoutMinutes(nextSummaryTimeoutMinutes);
    void invoke("save_preferences", { cleanupTempMedia: nextCleanup, transcriptionLanguage: nextLanguage, summaryTimeoutManual: nextSummaryTimeoutManual, summaryTimeoutMinutes: nextSummaryTimeoutMinutes }).then(() => setSaved("已保存")).catch(() => setSaved("保存失败"));
  };
  return (
    <main className="settings-page">
      <header className="config-topbar"><button type="button" onClick={onBack}>← 返回资料库</button><span>QUICKVIDEO · 设置</span></header>
      <section className="settings-layout">
        <aside><p className="eyebrow">PREFERENCES</p><h1>为你自己的<br /><em>阅读系统</em>设置规则。</h1><p>所有偏好都仅保存在本地设备。</p></aside>
        <div className="settings-list">
          <section><div><p>模型服务</p><span>SiliconFlow · 固定 API 端点</span></div><button type="button" onClick={onEditModel}>编辑 →</button></section>
          <section><div><p>本地转写模型</p><span>{runtime?.model ? "ggml-base 已就绪（约 142 MiB）" : "需要 ggml-base 模型，首次下载约 142 MiB"}</span></div>{runtime?.model ? <span className="runtime-ready">已就绪</span> : <button type="button" disabled={downloadingModel} onClick={() => { setDownloadingModel(true); setSaved("正在从官方模型仓库下载…"); void invoke("download_base_model").then(() => { setSaved("本地转写模型已下载。"); refreshRuntime(); }).catch((error) => setSaved(`模型下载失败：${String(error)}`)).finally(() => setDownloadingModel(false)); }}>{downloadingModel ? "下载中…" : "下载模型 →"}</button>}</section>
          <section><div><p>本地处理工具</p><span>{runtime ? `FFmpeg ${runtime.ffmpeg ? "✓" : "缺失"} · whisper.cpp ${runtime.whisper ? "✓" : "缺失"} · yt-dlp ${runtime.ytdlp ? "✓" : "缺失"}` : "正在检查运行环境…"}</span></div><button type="button" onClick={refreshRuntime}>重新检查</button></section>
          <section><div><p>默认转写语言</p><span>用于本地 whisper.cpp 的语言提示</span></div><select value={language} onChange={(event) => persist(cleanup, event.target.value)}><option>自动识别（中文 / English）</option><option>中文</option><option>English</option></select></section>
          <section><div><p>任务完成后删除临时媒体</p><span>只保留字幕、笔记和视频元数据</span></div><button className={`toggle ${cleanup ? "on" : ""}`} onClick={() => persist(!cleanup, language)} type="button" aria-label="切换清理临时媒体"><i /></button></section>
          <section className="model-timeout"><div><p>模型响应时长</p><span>测试 API Key 固定最多等待 30 秒</span><dl><div><dt>普通摘要生成</dt><dd>最长 {summaryTimeoutManual ? summaryTimeoutMinutes : 10} 分钟</dd></div><div><dt>流式摘要生成</dt><dd>最长 {summaryTimeoutManual ? summaryTimeoutMinutes : 10} 分钟</dd></div><div><dt>完整摘要任务</dt><dd>{summaryTimeoutManual ? `手动 · 最长 ${summaryTimeoutMinutes} 分钟` : "自动（推荐）· 最长 10 分钟"}</dd></div></dl></div><div className="timeout-controls"><label><span>手动设置</span><button className={`toggle ${summaryTimeoutManual ? "on" : ""}`} onClick={() => persist(cleanup, language, !summaryTimeoutManual)} type="button" aria-label="切换摘要等待时长手动设置"><i /></button></label>{summaryTimeoutManual && <select value={summaryTimeoutMinutes} onChange={(event) => persist(cleanup, language, true, Number(event.target.value))} aria-label="摘要最大等待时长">{[5, 10, 15, 20, 25, 30].map((minutes) => <option key={minutes} value={minutes}>{minutes} 分钟</option>)}</select>}</div></section>
          <section><div><p>笔记导出位置</p><span>每次导出时由 macOS 保存面板选择</span></div><span>Markdown</span></section>
          <section className="danger"><div><p>清空本地资料库</p><span>删除任务记录、字幕与笔记；不会影响原始视频。</span></div><button type="button" onClick={() => void ask("这会删除所有任务、字幕和笔记，且无法撤销。确定继续吗？", { title: "清空本地资料库", kind: "warning" }).then((accepted) => { if (accepted) onClearLibrary(); })}>清空资料库</button></section>
        </div>
        <section className="call-log" aria-labelledby="call-log-title">
          <div className="call-log-heading"><div><p className="step-label">MODEL TRACE</p><h2 id="call-log-title">最近调用记录</h2><span>保存完整请求与响应正文；Authorization 中的 API Key 始终脱敏。</span></div><button type="button" onClick={refreshCalls} disabled={callsLoading}>{callsLoading ? "刷新中…" : "刷新 ↻"}</button></div>
          {apiCalls.length === 0 ? <p className="call-log-empty">尚无模型调用记录。保存并测试连接，或重新生成摘要后会出现在这里。</p> : <div className="call-log-list">{apiCalls.map((call) => <details className={`call-row ${call.success ? "success" : "failed"}`} key={call.id}><summary><div className="call-state"><i />{call.success ? "成功" : "失败"}</div><div className="call-summary"><b>{call.purpose}</b><span>{call.model} · {call.durationMs < 1000 ? `${call.durationMs} ms` : `${(call.durationMs / 1000).toFixed(1)} s`}</span>{call.errorMessage && <code>{call.errorMessage}</code>}{call.traceId && <small>Trace ID · {call.traceId}</small>}</div><time title={call.createdAt}>{new Date(call.createdAt).toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false })}</time></summary><div className="call-payload"><div><b>请求头</b><pre>{call.requestHeaders ?? "旧记录未保存"}</pre></div><div><b>请求参数</b><pre>{call.requestBody ?? "旧记录未保存"}</pre></div><div><b>响应头</b><pre>{call.responseHeaders ?? "未收到 HTTP 响应"}</pre></div><div><b>响应正文</b><pre>{call.responseBody ?? "未收到 HTTP 响应"}</pre></div></div></details>)}</div>}
        </section>
        {saved && <p className="export-notice">{saved}</p>}
      </section>
    </main>
  );
}

type Caption = { time: string; text: string };

function parseCaptions(transcript: string): Caption[] {
  return transcript.replace(/\r/g, "").split(/\n\s*\n/).flatMap((block) => {
    const lines = block.split("\n").map((line) => line.trim()).filter(Boolean);
    const timestampIndex = lines.findIndex((line) => line.includes("-->"));
    if (timestampIndex < 0) return [];
    const text = lines.slice(timestampIndex + 1).join(" ");
    if (!text) return [];
    return [{ time: lines[timestampIndex].replace(/,\d{3}/g, ""), text }];
  });
}

function CaptionList({ captions }: { captions: Caption[] }) {
  return <ol className="caption-list">{captions.map((caption, index) => <li key={`${caption.time}-${index}`}><time>{caption.time}</time><span>{caption.text}</span></li>)}</ol>;
}

function NoteDetail({ task, onBack, onOpenSetup }: { task: VideoTask; onBack: () => void; onOpenSetup: () => void }) {
  const [exportNotice, setExportNotice] = useState("");
  const [savedNote, setSavedNote] = useState<{ transcript: string; summary: string } | null>(null);
  const [regenerating, setRegenerating] = useState(false);
  const [streamPreview, setStreamPreview] = useState("");
  const [streamStage, setStreamStage] = useState("正在准备请求…");
  const [transcriptExpanded, setTranscriptExpanded] = useState(false);
  useEffect(() => { invoke<[string, string]>("get_note", { taskId: task.id }).then(([transcript, summary]) => setSavedNote({ transcript, summary })).catch(() => setSavedNote(null)); }, [task.id]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<{ content: string; stage: string; reset: boolean }>("summary-stream", (event) => { setStreamStage(event.payload.stage); setStreamPreview((current) => event.payload.reset ? event.payload.content : current + event.payload.content); }).then((dispose) => { unlisten = dispose; });
    return () => unlisten?.();
  }, []);
  const captions = savedNote ? parseCaptions(savedNote.transcript) : [];
  const summaryPending = savedNote?.summary.startsWith("## 待生成摘要") ?? false;
  const visibleTitle = task.title.length > 80 ? `${task.title.slice(0, 80)}…` : task.title;
  const chapters = [
    ["00:00", "为什么收藏不等于吸收", "信息焦虑的根源不是输入太少，而是缺少把输入变成自己知识的过程。"],
    ["08:42", "四步学习工作流", "收集、理解、连接、输出：每一步都把内容向行动推进一点。"],
    ["23:16", "让系统长期运转", "通过固定回顾节奏与轻量输出，让知识库真正成为一个使用中的系统。"],
  ];
  const exportNote = async (exportType: ExportType) => {
    const exportLabel: Record<ExportType, string> = {
      summaryAndTranscript: "摘要和字幕",
      summary: "摘要",
      transcript: "字幕",
    };
    const target = await save({
      defaultPath: `${task.title}-${exportLabel[exportType]}.md`,
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (!target) return;
    try {
      const path = await invoke<string>("export_markdown", { title: task.title, taskId: task.id, outputPath: target, exportType });
      setExportNotice(`已导出到 ${path}`);
    } catch (error) {
      setExportNotice(`导出失败：${String(error)}`);
    }
  };
  const regenerateSummary = async () => {
    if (!savedNote) return;
    setStreamPreview("");
    setStreamStage("正在准备请求…");
    setRegenerating(true);
    try {
      const summary = await invoke<string>("generate_summary", { transcript: savedNote.transcript });
      await invoke("save_note", { taskId: task.id, transcript: savedNote.transcript, summary });
      setSavedNote({ ...savedNote, summary });
      setExportNotice("摘要已重新生成并保存到本地。");
    } catch (error) {
      setExportNotice(`无法生成摘要：${String(error)}`);
    } finally {
      setRegenerating(false);
    }
  };

  return (
    <main className="note-page">
      <header className="note-topbar">
        <button className="back-button" type="button" onClick={onBack}>← <span>返回资料库</span></button>
        <div className="note-actions">
          <span className="saved-state"><i /> 已保存到本地</span>
          {savedNote && <button type="button" onClick={() => void regenerateSummary()} disabled={regenerating}>{regenerating ? "正在生成…" : "重新生成摘要"}</button>}
          <button type="button" onClick={() => void exportNote("summaryAndTranscript")}>导出摘要和字幕 ↓</button>
          <button type="button" onClick={() => void exportNote("summary")}>导出摘要 ↓</button>
          <button type="button" onClick={() => void exportNote("transcript")}>导出字幕 ↓</button>
        </div>
      </header>

      <div className="note-layout">
        <aside className="chapter-nav">
          <p className="step-label">VIDEO MAP</p>
          <strong>本段视频</strong>
          <p className="note-duration">{task.duration} · {task.subtitleSource ?? "字幕来源待确认"}</p>
          {savedNote ? <p className="chapter-ready">章节时间范围已在右侧生成笔记中保留。</p> : <ol>
              {chapters.map(([time, title], index) => (
                <li className={index === 0 ? "current" : ""} key={time}>
                  <button type="button"><span>{time}</span>{title}</button>
                </li>
              ))}
            </ol>}
          <div className="source-stamp">SOURCE<br /><b>{task.source.split("·")[0]}</b></div>
        </aside>

        <article className="note-content">
          <p className="eyebrow">VIDEO NOTE · {formatTaskTime(task.createdAt)}</p>
          <h1 title={task.title}>{visibleTitle}</h1>
          <p className="note-subtitle">一份可以替你重看视频的结构化笔记。</p>

          {savedNote ? <>
            {regenerating && <section className="stream-summary"><p className="step-label">{streamStage}</p><pre>{streamPreview || "正在等待模型返回第一个字符…"}</pre><span>{streamStage.includes("最终") ? "中间提炼完成后，正在生成最终笔记" : "中间章节提炼实时显示；完成后会自动替换为最终笔记"} · 最多等待 10 分钟</span></section>}
            {summaryPending ? <section className="summary-pending">
              <p className="step-label">摘要待生成</p>
              <h2>字幕已经完成。</h2>
              <p>配置你的模型服务后，即可根据这份字幕生成章节、概览和核心收获。</p>
              <button type="button" onClick={onOpenSetup}>去配置模型 →</button>
            </section> : <section className="saved-summary"><p className="step-label">已生成笔记</p><pre>{savedNote.summary}</pre></section>}
            <section className="transcript-teaser">
              <div><span>字幕</span><p>完整字幕</p></div>
              {captions.length > 0 ? <CaptionList captions={transcriptExpanded ? captions : captions.slice(0, 4)} /> : <p className={transcriptExpanded ? "full-transcript" : ""}>{transcriptExpanded ? savedNote.transcript : savedNote.transcript.slice(0, 500)}</p>}
              {(captions.length > 4 || (!captions.length && savedNote.transcript.length > 500)) && <button type="button" onClick={() => setTranscriptExpanded((expanded) => !expanded)}>{transcriptExpanded ? "收起全文字幕" : "展开全文字幕"} <span>{transcriptExpanded ? "↑" : "↓"}</span></button>}
            </section>
          </> : <>
          <section className="thesis-block">
            <span>示例：一句话概览</span>
            <p>导入后，这里会出现基于字幕生成的真实视频概要。</p>
          </section>

          <section className="takeaways">
            <div className="section-kicker"><span>01</span><p>核心收获</p></div>
            <ul>
              <li><b>01</b><span>不要把收藏当作学习完成，真正的理解发生在自己的复述与使用之后。</span></li>
              <li><b>02</b><span>每条重要内容都需要被放回一个具体问题或行动之中。</span></li>
              <li><b>03</b><span>知识库的价值来自持续复用，而不来自资料数量。</span></li>
            </ul>
          </section>

          <section className="chapter-notes">
            <div className="section-kicker"><span>02</span><p>章节笔记</p></div>
            {chapters.map(([time, title, summary], index) => (
              <article className="chapter-entry" key={time}>
                <div className="chapter-time">{time}<i /></div>
                <div>
                  <h2>{title}</h2>
                  <p>{summary}</p>
                  {index === 1 && <div className="quote">“好的学习系统，不是让你读得更多，而是让你更容易再次使用。”</div>}
                </div>
              </article>
            ))}
          </section>

          <section className="transcript-teaser">
            <div><span>03</span><p>字幕节选</p></div>
            <p><b>[00:00:12]</b> 今天想和大家聊聊，为什么我们收藏了那么多内容，却很少真的把它们变成自己的能力。</p>
            <button type="button">展开全文字幕 <span>↓</span></button>
          </section>
          </>}
          {exportNotice && <p className="export-notice">{exportNotice}</p>}
        </article>
      </div>
    </main>
  );
}

export default App;
