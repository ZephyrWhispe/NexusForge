import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import KvmPanel from "../KvmPanel";
import {
  kvmControlState,
  kvmDiscoveredPeers,
  kvmEdgeMap,
  kvmIssuePairCode,
  kvmPairedPeers,
  kvmSendClip,
  kvmSendFile,
  kvmSessionList,
  type PairedPeerDto,
  type SessionDto,
} from "../../../ipc/client";

// D-29 B1/T-B1-7 回归：推送剪贴板/推送文件仅认 role=client 出站会话（核账⑤），
// readText 被拒不弹裸报错而是回落「推送文本」对话框，「活跃会话」卡按角色呈现。

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    kvmDiscoveredPeers: vi.fn(),
    kvmPairedPeers: vi.fn(),
    kvmSessionList: vi.fn(),
    kvmControlState: vi.fn(),
    kvmEdgeMap: vi.fn(),
    kvmIssuePairCode: vi.fn(),
    kvmSendClip: vi.fn(),
    kvmSendFile: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

const peer = (id: string, name: string): PairedPeerDto => ({
  device_id: id,
  device_name: name,
  fingerprint: `fp-${id}-0123456789`,
  pubkey_b64: "cHVi",
  paired_at: 0,
});
const session = (id: string, name: string, role: "client" | "server"): SessionDto => ({
  device_id: id,
  device_name: name,
  role,
});

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string, scope: ParentNode = container): HTMLButtonElement | undefined {
  return [...scope.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}

function buttonsByText(text: string, scope: ParentNode = container): HTMLButtonElement[] {
  return [...scope.querySelectorAll("button")].filter((b) => b.textContent?.trim() === text);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<KvmPanel />);
  });
  await act(async () => {});
}

function setClipboard(readText: (() => Promise<string>) | undefined) {
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: readText ? { readText } : undefined,
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(kvmDiscoveredPeers).mockResolvedValue([]);
  vi.mocked(kvmPairedPeers).mockResolvedValue([peer("devA", "设备 A"), peer("devB", "设备 B")]);
  vi.mocked(kvmSessionList).mockResolvedValue([]);
  vi.mocked(kvmControlState).mockResolvedValue({ role: "idle" });
  vi.mocked(kvmEdgeMap).mockResolvedValue({});
  // ttl=0：跳过续签定时器，测试内不起 2 分钟倒计时
  vi.mocked(kvmIssuePairCode).mockResolvedValue(["123456", 0]);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  container.remove();
  vi.clearAllMocks();
});

describe("KvmPanel 推送门禁 + 回落 + 会话表（T-B1-7）", () => {
  it("kvmPush_buttonsGatedOnClientRoleOnly：server 会话行推送禁用且零 invoke（核账⑤）", async () => {
    // devA=本端发起（可推送），devB=对端接入（不是推送目标）
    vi.mocked(kvmSessionList).mockResolvedValue([
      session("devA", "设备 A", "client"),
      session("devB", "设备 B", "server"),
    ]);
    await mount();
    const clipBtns = buttonsByText("推送剪贴板");
    const fileBtns = buttonsByText("推送文件");
    expect(clipBtns).toHaveLength(2);
    expect(fileBtns).toHaveLength(2);
    expect(clipBtns[0].disabled).toBe(false);
    // jsdom 对 disabled 按钮仍会派发 onClick，故门禁双保险：按钮 disabled +
    // 处理器首行 client 会话守卫，机器判据=禁用态+tooltip+点击后零 invoke。
    expect(clipBtns[1].disabled).toBe(true);
    expect(clipBtns[1].title).toContain("对端接入的会话不视为推送目标");
    expect(fileBtns[1].disabled).toBe(true);
    await click(clipBtns[1]);
    await click(fileBtns[1]);
    expect(kvmSendClip).not.toHaveBeenCalled();
    expect(kvmSendFile).not.toHaveBeenCalled();
    // 禁用态不得顺手打开任何对话框
    expect(document.querySelector("textarea")).toBeNull();
  });

  it("kvmPush_readTextDenied_fallsBackToDialog：NotAllowedError→对话框→确认后收到 {Text:{text,html:null}}", async () => {
    vi.mocked(kvmSessionList).mockResolvedValue([session("devA", "设备 A", "client")]);
    const denied = new Error("denied");
    denied.name = "NotAllowedError";
    setClipboard(vi.fn(async () => Promise.reject(denied)));
    await mount();
    await click(buttonByText("推送剪贴板")!);
    expect(kvmSendClip).not.toHaveBeenCalled();
    // 回落对话框出现，并向用户解释为什么不是直接推送
    expect(document.body.textContent).toContain("自动读取本机剪贴板被系统拒绝");
    expect(document.body.textContent).toContain("此路径不需要任何剪贴板权限");
    const ta = document.querySelector("textarea");
    expect(ta).not.toBeNull();
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
    setter.call(ta, "手动粘贴的文本");
    ta!.dispatchEvent(new Event("input", { bubbles: true }));
    await act(async () => {});
    await click(buttonByText("推送", document.body)!);
    // serde 外部 tagging 字面形状 + html 键必带（Rust Option 缺键即反序列化失败）
    expect(kvmSendClip).toHaveBeenCalledTimes(1);
    expect(kvmSendClip).toHaveBeenCalledWith("devA", {
      Text: { text: "手动粘贴的文本", html: null },
    });
  });

  it("kvmSessionTable_rendersRoleBadge：活跃会话卡按角色出徽标，任意角色仍算「会话中」", async () => {
    vi.mocked(kvmSessionList).mockResolvedValue([
      session("devA", "设备 A", "client"),
      session("devB", "设备 B", "server"),
    ]);
    // devB 在线且只有 server 会话：连接互斥仍须生效（任意角色语义不因拆分而变）
    vi.mocked(kvmDiscoveredPeers).mockResolvedValue([
      {
        device_id: "devB",
        device_name: "设备 B",
        pubkey_fingerprint: "fp-devB-0123456789",
        tcp_port: 5900,
        caps: [],
        addr: "10.0.0.5:5900",
        screen: { x: 0, y: 0, w: 1920, h: 1080 },
      },
    ]);
    await mount();
    expect(container.textContent).toContain("活跃会话");
    expect(container.textContent).toContain("client · 本端发起");
    expect(container.textContent).toContain("server · 对端接入");
    // 既有语义回归位：「会话中」徽标与连接互斥仍按任意角色
    expect(container.textContent).toContain("会话中");
    const connBtns = buttonsByText("连接");
    expect(connBtns[1].disabled).toBe(true);
  });
});
