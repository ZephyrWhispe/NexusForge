import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import FilePanel from "../FilePanel";
import ConnectionsSection from "../ConnectionsSection";
import RemoteBrowser from "../RemoteBrowser";
import TofuPromptDialog, { parseTofuRequest } from "../TofuPromptDialog";
import { connectButtonState } from "../connectFlow";
import {
  fileBreadcrumbs,
  fileDrives,
  fileEnqueue,
  fileOpResume,
  fileOpsActive,
  fileOpsPending,
  filePreview,
  fileRemoteBrowse,
  fileRemoteChmod,
  fileRemoteConnect,
  fileRemoteDisconnect,
  fileRemoteDrivers,
  fileRemoteFingerprintAck,
  fileRemoteProfileDelete,
  fileRemoteProfileSave,
  fileRemoteProfiles,
  fileRemotePresets,
  fileSearch,
  fileList,
  xferStatus,
  type FileEntryDto,
  type OpProgressDto,
  type RemoteDriverDto,
  type RemotePresetDto,
  type RemoteProfileDto,
} from "../../../ipc/client";
import { useSession } from "../../../stores/session";

// D-29 B6/T-B6-10 回归（09 §6.2）：连接与授权 UI 三块——①换协议=整表单重挂载
// （旧字段值不残留的负例）②三态钮唯一决策点（connecting 折叠不可能态）
// ③TOFU 对话框逐字复述后端两枚错误消息（首见可 ack 恰一次；键变更只有取消）。
// 外加：auth_source 列只报来源不值、档案面零凭据字段（query + 持久层两侧）、
// 远端浏览 roots 只出自 file_remote_drivers（本地盘符禁入）、文件三档分派。

const listenMock = vi.fn();

vi.mock("@tauri-apps/api/event", () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    fileList: vi.fn(),
    fileBreadcrumbs: vi.fn(),
    fileDrives: vi.fn(),
    fileSearch: vi.fn(),
    filePreview: vi.fn(),
    fileEnqueue: vi.fn(),
    fileOpsActive: vi.fn(),
    fileOpsPending: vi.fn(),
    fileOpResume: vi.fn(),
    xferStatus: vi.fn(),
    fileRemoteProfiles: vi.fn(),
    fileRemoteProfileSave: vi.fn(),
    fileRemoteProfileDelete: vi.fn(),
    fileRemotePresets: vi.fn(),
    fileRemoteDrivers: vi.fn(),
    fileRemoteBrowse: vi.fn(),
    fileRemoteChmod: vi.fn(),
    fileRemoteConnect: vi.fn(),
    fileRemoteFingerprintAck: vi.fn(),
    fileRemoteDisconnect: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

function profileFixture(over: Partial<RemoteProfileDto> = {}): RemoteProfileDto {
  return {
    id: "prof-1",
    label: "归档站",
    protocol: "sftp",
    host: "10.0.0.5",
    port: 22,
    user: "me",
    base_path: "/",
    auth: { kind: "anonymous" },
    preset_id: null,
    last_used_ms: 0,
    ...over,
  };
}

function driverFixture(over: Partial<RemoteDriverDto> = {}): RemoteDriverDto {
  return {
    driver_id: "prof-1",
    label: "归档站",
    protocol: "sftp",
    host: "10.0.0.5",
    port: 22,
    base_path: "/",
    roots: ["/"],
    auth_source: "key_file",
    ...over,
  };
}

function presetFixture(over: Partial<RemotePresetDto> = {}): RemotePresetDto {
  return {
    id: "preset-sftp",
    label: "SFTP",
    protocol: "sftp",
    default_host: "",
    port: 22,
    base_path: "/srv",
    auth_kind: "anonymous",
    notes: "",
    ...over,
  };
}

function entry(name: string, isDir: boolean): FileEntryDto {
  return {
    name,
    path: `C:\\dir\\${name}`,
    is_dir: isDir,
    size: isDir ? 0 : 1024,
    modified_ms: Date.parse("2026-09-19T10:00:00"),
    ext: isDir ? "" : name.split(".").pop() ?? "",
    hidden: false,
  };
}

function opRow(opId: string, over: Partial<OpProgressDto> = {}): OpProgressDto {
  return {
    op_id: opId,
    kind: "copy",
    state: "running",
    current: "big.bin",
    files_done: 1,
    files_total: 2,
    bytes_done: 1024,
    bytes_total: 4096,
    error: null,
    direction: "local",
    resumable: null,
    resumed_from: null,
    ...over,
  };
}

/** TOFU 消息夹具：与 ssh.rs:397-408 tofu_guard 的成品文案逐字同形 */
const FP = "ed25519 SHA256:Q3rVmZ8xL0kP9tW2sFhYd7buN1cE4gJ6oI8eR0aT5vU";
const UNKNOWN_MSG = `主机 10.0.0.5:22 首次连接，服务器密钥不在记录，连接已拒。指纹（逐字）：${FP}。请经带外渠道与服务器侧核对这枚指纹完全一致后，经指纹确认命令明示记录它；本客户端没有隐式自纳这回事`;
const FP_OLD = "ed25519 SHA256:0000oldRecordedKeyFingerprintAAAAAAAAAAAAAAAAAAA";
const FP_NEW = "ed25519 SHA256:9999actualArrivedKeyFingerprintBBBBBBBBBBBBBBBB";
const CHANGED_MSG = `主机 10.0.0.5:22 密钥已变更，连接已拒。记录（逐字）：${FP_OLD}。实收（逐字）：${FP_NEW}。若确认主机已重建，先删除该主机的记录再发起连接；不存在带着旧记录继续连的出路`;

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}
function buttonByContains(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) =>
    b.textContent?.includes(text),
  );
}
function rowByText(text: string): Element | undefined {
  return [...container.querySelectorAll("tr")].find((r) => r.textContent?.includes(text));
}
function inputById(id: string): HTMLInputElement {
  return document.getElementById(id) as HTMLInputElement;
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function setInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  await act(async () => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function setSelect(el: HTMLSelectElement, value: string) {
  await act(async () => {
    el.value = value;
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function mount(element: React.ReactNode) {
  await act(async () => {
    root = createRoot(container);
    root.render(<>{element}</>);
  });
  await act(async () => {});
}

function unmount() {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  delete (window as { __TAURI_INTERNALS__?: object }).__TAURI_INTERNALS__;
  listenMock.mockReset();
  listenMock.mockResolvedValue(() => {});
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(fileRemoteProfiles).mockResolvedValue([]);
  vi.mocked(fileRemoteDrivers).mockResolvedValue([]);
  vi.mocked(fileRemotePresets).mockResolvedValue([]);
  vi.mocked(fileRemoteProfileSave).mockImplementation(async (p) => p);
  vi.mocked(fileRemoteProfileDelete).mockResolvedValue(true);
  vi.mocked(fileRemoteFingerprintAck).mockResolvedValue(undefined);
  vi.mocked(fileRemoteDisconnect).mockResolvedValue(true);
  vi.mocked(fileRemoteBrowse).mockResolvedValue([]);
  vi.mocked(fileRemoteChmod).mockResolvedValue(undefined);
  vi.mocked(fileRemoteConnect).mockResolvedValue(driverFixture());
  // FilePanel 侧缺省（三档分派测要用）
  vi.mocked(fileList).mockResolvedValue([entry("docs", true), entry("a.txt", false)]);
  vi.mocked(fileBreadcrumbs).mockResolvedValue([
    ["C:", "C:\\"],
    ["dir", "C:\\dir"],
  ]);
  vi.mocked(fileDrives).mockResolvedValue([]);
  vi.mocked(fileSearch).mockResolvedValue({ hits: [], degraded: false });
  vi.mocked(filePreview).mockResolvedValue({ kind: "unsupported", reason: "未配置" });
  vi.mocked(fileOpsActive).mockResolvedValue([]);
  vi.mocked(fileOpsPending).mockResolvedValue([]);
  vi.mocked(fileEnqueue).mockResolvedValue({ op_id: "op-new", conflicts: [], name_fix: [] });
  vi.mocked(fileOpResume).mockResolvedValue({ op_id: "op-resumed", previous_op_id: "op-old" });
  vi.mocked(xferStatus).mockResolvedValue(opRow("op-resumed"));
});

afterEach(() => {
  unmount();
  container.remove();
  useSession.setState({ fileSubPanel: "browse" });
  vi.clearAllMocks();
});

describe("连接表单重挂载（T-B6-10）", () => {
  it("connectDialog_protocolSwitch_rerendersEntireForm：换协议整表单重挂载，旧字段值不残留", async () => {
    vi.mocked(fileRemotePresets).mockResolvedValue([presetFixture()]);
    await mount(<ConnectionsSection />);
    const oldForm = document.querySelector('[data-testid="conn-form-web_dav"]');
    expect(oldForm?.isConnected).toBe(true);
    await setInput(inputById("cs-host"), "泄漏LEAK-oldhost");
    await setInput(inputById("cs-label"), "泄漏LEAK-alias");

    await setSelect(document.getElementById("cs-proto") as HTMLSelectElement, "sftp");

    // DOM 级证据：旧 section 脱离文档，新协议表单从零开始
    expect(oldForm?.isConnected).toBe(false);
    const newForm = document.querySelector('[data-testid="conn-form-sftp"]');
    expect(newForm?.isConnected).toBe(true);
    // 负例：旧字段值不残留
    expect(inputById("cs-host").value).toBe("");
    expect(inputById("cs-label").value).toBe("");
    // 正对照：端口取 file_remote_presets 事实源（sftp ⇒ 22），不是沿用缺省 80
    expect(inputById("cs-port").value).toBe("22");
  });
});

describe("三态钮唯一决策点（T-B6-10）", () => {
  it("connectButtonState_threeStatesExactlyOnePrimary：四入臂纯函数 + idle→connecting→connected 同屏互斥", () => {
    expect(connectButtonState({ connecting: false, connected: false })).toBe("idle");
    expect(connectButtonState({ connecting: true, connected: false })).toBe("connecting");
    expect(connectButtonState({ connecting: false, connected: true })).toBe("connected");
    // 不可能态折叠：connecting 与 connected 并存只得 connecting——转圈与绿勾不同屏
    expect(connectButtonState({ connecting: true, connected: true })).toBe("connecting");
  });

  it("connectButtonState_threeStatesExactlyOnePrimary_dom：测试连接/连接中…/断开同屏恰一枚", async () => {
    vi.mocked(fileRemoteProfiles).mockResolvedValue([profileFixture()]);
    // 挂载期两个读者（RemoteBrowser 子效应 + ConnectionsSection 自身）都读到空表，
    // 连接成功后的第三读（reloadDrivers）才见 prof-1——否则选档即 connected，测不到 idle
    vi.mocked(fileRemoteDrivers)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([])
      .mockResolvedValue([driverFixture()]);
    let release!: (v: RemoteDriverDto) => void;
    vi.mocked(fileRemoteConnect).mockImplementationOnce(
      () => new Promise<RemoteDriverDto>((res) => (release = res)),
    );
    await mount(<ConnectionsSection />);
    const actionLabels = () =>
      ["测试连接", "连接中…", "断开"].filter((l) => buttonByText(l) !== undefined);

    // idle：只有 primary 测试连接
    expect(actionLabels()).toEqual(["测试连接"]);
    await click(buttonByContains("归档站")!); // 选中档案，draft.id = prof-1

    await click(buttonByText("测试连接")!);
    // connecting：唯一在场的是禁用中的转圈钮
    const busy = buttonByText("连接中…");
    expect(busy).toBeDefined();
    expect(busy!.disabled).toBe(true);
    expect(actionLabels()).toEqual(["连接中…"]);
    // 绿勾臂不在场（"已连接"子串会被远端浏览空态"尚无已连接远端"撞词，改钉 RTT 专属证据）
    expect(document.body.textContent).not.toContain("RTT");

    await act(async () => release(driverFixture()));
    // connected：断开钮接班，绿勾在场且带实测 RTT
    expect(actionLabels()).toEqual(["断开"]);
    expect(document.body.textContent).toContain("已连接");
    expect(document.body.textContent).toMatch(/RTT \d+ms/);
  });
});

describe("TOFU 对话框（T-B6-10）", () => {
  it("tofuDialog_firstSight_showsFingerprintVerbatim_andAcksOnce：逐字复述 + 接受恰一次 + 取消臂零调用", async () => {
    const parsed = parseTofuRequest("FILE_REMOTE_001", UNKNOWN_MSG);
    expect(parsed).toEqual({ arm: "unknown", host: "10.0.0.5", port: 22, fingerprint: FP });
    // 同为 001 的非 TOFU 形态（档案不存在等）不得误开 TOFU 门
    expect(parseTofuRequest("FILE_REMOTE_001", "档案不存在：remote:zz")).toBeNull();

    const onAck = vi.fn();
    const onCancel = vi.fn();
    await mount(
      <TofuPromptDialog request={parsed} open onCancel={onCancel} onAck={onAck} />,
    );
    // 指纹逐字等夹具：不归一化、不截断
    expect(document.body.textContent).toContain(FP);
    expect(document.body.textContent).toContain("10.0.0.5:22");
    await click(buttonByText("接受并连接")!);
    expect(onAck).toHaveBeenCalledTimes(1);
    expect(onAck).toHaveBeenCalledWith(FP);
    unmount();

    // 取消臂：ack 零调用
    const onAck2 = vi.fn();
    const onCancel2 = vi.fn();
    await mount(
      <TofuPromptDialog request={parsed} open onCancel={onCancel2} onAck={onAck2} />,
    );
    await click(buttonByText("取消")!);
    expect(onAck2).not.toHaveBeenCalled();
    expect(onCancel2).toHaveBeenCalled();
  });

  it("tofuDialog_changedKey_offersCancelOnly：两枚指纹逐字 + 只有取消，无“仍然连接”出路", async () => {
    const parsed = parseTofuRequest("FILE_REMOTE_005", CHANGED_MSG);
    expect(parsed).toEqual({
      arm: "changed",
      host: "10.0.0.5",
      port: 22,
      recorded: FP_OLD,
      actual: FP_NEW,
    });
    const onAck = vi.fn();
    const onCancel = vi.fn();
    await mount(
      <TofuPromptDialog request={parsed} open onCancel={onCancel} onAck={onAck} />,
    );
    expect(document.body.textContent).toContain(FP_OLD);
    expect(document.body.textContent).toContain(FP_NEW);
    expect(document.body.textContent).toContain("连接已拒");
    // UI 不得给出后端已拒绝的承诺：Changed 臂有字按钮全集恰一枚取消
    expect(
      [...document.querySelectorAll("button")]
        .map((b) => b.textContent?.trim() ?? "")
        .filter((t) => t !== ""),
    ).toEqual(["取消"]);
    expect(buttonByText("接受并连接")).toBeUndefined();
    expect(document.body.textContent).not.toMatch(/仍然连接|忽略|信任并记录/);
    await click(buttonByText("取消")!);
    expect(onAck).not.toHaveBeenCalled();
    expect(onCancel).toHaveBeenCalled();
  });
});

describe("auth_source 列与零凭据档案面（T-B6-10）", () => {
  it("authSourceColumn_showsSourceNeverValue：列表只报来源档位，指针 id 只进各自档位输入", async () => {
    vi.mocked(fileRemoteProfiles).mockResolvedValue([
      profileFixture({ id: "pv", label: "保险库站", auth: { kind: "vault_entry", entry_id: "ent-upload-7" } }),
      profileFixture({ id: "pk", label: "密钥站", auth: { kind: "ssh_key", key_path: "C:\\keys\\id_ed25519" } }),
      profileFixture({ id: "pt", label: "逐次站", auth: { kind: "prompt_each_time" } }),
    ]);
    await mount(<ConnectionsSection />);
    const text = document.body.textContent ?? "";
    // 只报来源档位名（五档受控词表之三）
    expect(text).toContain("VaultEntry");
    expect(text).toContain("KeyFile");
    expect(text).toContain("Typed");
    // 列表行不渲染指针载荷
    expect(text).not.toContain("ent-upload-7");
    expect(text).not.toContain("id_ed25519");
    // 面板零口令输入：口令只活在连接对话框
    expect(document.querySelector('input[type="password"]')).toBeNull();

    await click(buttonByContains("保险库站")!);
    // VaultEntry 臂正对照：条目 id 是指针，可在其档位输入框在场
    expect(inputById("cs-entryid").value).toBe("ent-upload-7");
    await click(buttonByContains("密钥站")!);
    expect(inputById("cs-keypath").value).toBe("C:\\keys\\id_ed25519");
    expect(inputById("cs-entryid") ?? null).toBeNull();
  });

  it("connectionsPanel_savedProfile_hasNoSecretInQuery：口令只进 file_remote_connect 一道门，query/持久层两侧零凭据", async () => {
    vi.mocked(fileRemoteProfiles).mockResolvedValue([
      profileFixture({ id: "pt", label: "逐次站", auth: { kind: "prompt_each_time" } }),
    ]);
    await mount(<ConnectionsSection />);
    await click(buttonByContains("逐次站")!);
    await click(buttonByText("测试连接")!); // typed 档 ⇒ 只开连接对话框，本表单零口令字段
    await setInput(inputById("cd-pass"), "SUPER_SECRET");
    await click(buttonByText("连接")!);

    // 唯一入口：口令作为 AuthSecretDto 进 connect 命令
    expect(fileRemoteConnect).toHaveBeenCalledWith("pt", { password: "SUPER_SECRET" }, false);
    // query 侧：其余所有远端命令的实参逐枚扫，无 password/secret 字段名，更无口令值
    for (const fn of [fileRemoteProfileSave, fileRemoteProfiles, fileRemoteFingerprintAck]) {
      for (const call of vi.mocked(fn).mock.calls) {
        const json = JSON.stringify(call);
        expect(json).not.toMatch(/password|secret|passphrase|SUPER_SECRET/i);
      }
    }
    // 呈现侧：DOM 全文不回显口令
    expect(document.body.textContent).not.toContain("SUPER_SECRET");
    // 持久层侧：localStorage 全体（含 nf-session）零凭据痕迹
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i)!;
      expect(`${key}:${localStorage.getItem(key) ?? ""}`).not.toMatch(/password|secret|SUPER_SECRET/i);
    }
  });
});

describe("远端浏览事实源（T-B6-10 承重⑮）", () => {
  it("remoteBrowser_rootsComeFromRemoteDrivers_notDriveLetters：roots 只出自 file_remote_drivers，本地盘符零进表", async () => {
    vi.mocked(fileRemoteDrivers).mockResolvedValue([
      driverFixture({ driver_id: "dv", label: "联调远端", roots: ["/", "/pub"], auth_source: "session" }),
    ]);
    await mount(<RemoteBrowser />);
    const options = [...document.querySelectorAll("select option")].map((o) => o.textContent?.trim());
    expect(options).toContain("联调远端");
    expect(options).toContain("/");
    expect(options).toContain("/pub");
    // 负例：任何 option 都不是盘符形状
    for (const t of options) expect(t).not.toMatch(/^[A-Za-z]:/);
    // 本地盘符表从未被触碰（混表即把未连接的远端假称在场）
    expect(fileDrives).not.toHaveBeenCalled();
    // auth_source 只报来源档位
    expect(document.body.textContent).toContain("凭据来源 session");
  });
});

describe("远端权限位面（T-B7-25）", () => {
  it("propertiesPanel_showsOctalAndSymlinkTarget：mode 在场渲八进制+链接目标，mode=None 两行不渲", async () => {
    vi.mocked(fileRemoteDrivers).mockResolvedValue([
      driverFixture({ driver_id: "dv", roots: ["/srv"], protocol: "sftp" }),
    ]);
    vi.mocked(fileRemoteBrowse).mockResolvedValue([
      {
        name: "app.sh",
        path: "/srv/app.sh",
        is_dir: false,
        size: 8,
        modified_ms: 0,
        mode: 0o755,
        symlink_target: "/usr/bin/sh",
      },
      {
        name: "plain.txt",
        path: "/srv/plain.txt",
        is_dir: false,
        size: 1,
        modified_ms: 0,
        mode: null,
        symlink_target: null,
      },
    ]);
    await mount(<RemoteBrowser />);
    await click(buttonByText("浏览")!);

    // ① mode/symlink 在场：两行上屏（0o 前缀显式，不裸写三位让人猜进制）
    await click(rowByText("app.sh")!.querySelector("[data-properties]")!);
    expect(document.body.textContent).toContain("权限位（八进制）0o755");
    expect(document.body.textContent).toContain("符号链接目标 /usr/bin/sh");
    // ② 越界输入本地即拒（后端算式闸是第二道，不是唯一 UX）
    await setInput(inputById("remote-props-octal"), "97777");
    await click(buttonByText("写回")!);
    expect(fileRemoteChmod).not.toHaveBeenCalled();
    expect(document.body.textContent).toContain("1-4 位");
    // ③ 合法写回：唯一口逐字对参，成功后关窗并重新列目录（显示即事实）
    await setInput(inputById("remote-props-octal"), "644");
    await click(buttonByText("写回")!);
    expect(fileRemoteChmod).toHaveBeenCalledWith("dv", "/srv/app.sh", 0o644);
    expect(document.body.textContent).not.toContain("属性：app.sh");
    expect(fileRemoteBrowse).toHaveBeenCalledTimes(2);
    unmount();
  });

  it("propertiesPanel_modeNull_rendersNoOctalNoSymlinkRows：无事实源两行不渲染", async () => {
    vi.mocked(fileRemoteDrivers).mockResolvedValue([
      driverFixture({ driver_id: "dv", roots: ["/srv"], protocol: "webdav" }),
    ]);
    vi.mocked(fileRemoteBrowse).mockResolvedValue([
      {
        name: "plain.txt",
        path: "/srv/plain.txt",
        is_dir: false,
        size: 1,
        modified_ms: 0,
        mode: null,
        symlink_target: null,
      },
    ]);
    await mount(<RemoteBrowser />);
    await click(buttonByText("浏览")!);
    await click(rowByText("plain.txt")!.querySelector("[data-properties]")!);
    // 正对照：弹窗本体在场（不是整个属性面都没立）
    expect(document.body.textContent).toContain("属性：plain.txt");
    // mode=None：八进制行与可编辑位不渲染、链接目标行不渲染——只给归因句
    expect(document.body.textContent).not.toContain("权限位（八进制）");
    expect(document.body.textContent).not.toContain("符号链接目标");
    expect(document.body.textContent).not.toContain("写回");
    expect(document.body.textContent).toContain("无权限位事实源");
    unmount();
  });
});

describe("文件三档分派（T-B6-10）", () => {
  it("fileSubPanel_threeWayDispatch_movesExistingViewsUnchanged：三档各自在场且互不越档，首屏不空", async () => {
    vi.mocked(fileOpsActive).mockResolvedValue([opRow("disp-1")]);

    // 浏览档：目录表 + 面包屑在场，队列/连接臂不渲染
    useSession.setState({ fileSubPanel: "browse" });
    await mount(<FilePanel />);
    expect(rowByText("a.txt")).toBeDefined();
    expect(container.textContent).toContain("C:");
    expect(container.textContent).toContain("dir");
    expect(container.querySelector('[data-op-id="disp-1"]')).toBeNull();
    expect(container.textContent).not.toContain("站点");
    expect((container.textContent ?? "").length).toBeGreaterThan(0);
    unmount();

    // 传输档：T-B6-9 队列原样搬入（行徽标即"未改动"证据），目录表不渲染
    useSession.setState({ fileSubPanel: "transfers" });
    await mount(<FilePanel />);
    expect(container.querySelector('[data-op-id="disp-1"]')).not.toBeNull();
    expect(container.textContent).toContain("进行中");
    expect(rowByText("a.txt")).toBeUndefined();
    expect(container.textContent).not.toContain("站点");
    unmount();

    // 连接档：ConnectionsSection（含 RemoteBrowser）在场，队列/目录不渲染
    useSession.setState({ fileSubPanel: "connections" });
    await mount(<FilePanel />);
    expect(container.textContent).toContain("站点");
    expect(container.textContent).toContain("远端浏览");
    expect(buttonByText("新建")).toBeDefined();
    expect(container.querySelector('[data-op-id="disp-1"]')).toBeNull();
    expect(rowByText("a.txt")).toBeUndefined();
    unmount();

    // 空队列的传输档不假空：给的是引导文案而非白板
    vi.mocked(fileOpsActive).mockResolvedValue([]);
    useSession.setState({ fileSubPanel: "transfers" });
    await mount(<FilePanel />);
    expect(container.textContent).toContain("当前没有进行中或近期的传输");
  });
});
