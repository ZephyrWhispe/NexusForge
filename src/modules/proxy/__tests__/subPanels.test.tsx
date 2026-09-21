import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ProxyPanel from "../ProxyPanel";
import {
  proxyKernelInstall,
  proxyKernelSelect,
  proxyLogs,
  proxyNodes,
  proxyStatus,
  type ProxyNodeDto,
  type ProxyStatusDto,
} from "../../../ipc/client";
import { confirmAction, impactLines, type ConfirmOptions } from "../../../stores/confirm";
import { useSession } from "../../../stores/session";

// D-29 B2/T-B2-3 六枚任务书字面回归（09 §5.2）：子面板化骨架、内核卡安装扩参、
// 换核红线负例（取消零 invoke）、协议兼容预检文案、日志级别纯前端过滤零新
// invoke、proxySubPanel 持久化往返（partialize 零迁移）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    proxyStatus: vi.fn(),
    proxySubs: vi.fn(async () => [
      { id: "s1", name: "主订阅", url: "https://example.test/sub", updated_ms: 1, node_count: 3 },
    ]),
    proxyNodes: vi.fn(async () => NODES),
    proxyDirectRules: vi.fn(async () => ["bilibili.com"]),
    proxyLogs: vi.fn(async () => LOGS),
    proxyKernelSelect: vi.fn(async () => {}),
    proxyKernelInstall: vi.fn(async () => ({})),
    proxyKernelRestart: vi.fn(async () => {}),
    proxyWintunInstall: vi.fn(async () => {}),
    proxySetMode: vi.fn(async () => {}),
    proxyDelayTest: vi.fn(async () => []),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

const NODES: ProxyNodeDto[] = [
  { tag: "HK-A", kind: "vmess", server: "1.2.3.4", port: 443, sub_id: "s1", groups: ["HK 分组"] },
  { tag: "HK-B", kind: "shadowsocks", server: "5.6.7.8", port: 8388, sub_id: "s1", groups: [] },
  { tag: "TO-HY2", kind: "hysteria2", server: "9.10.11.12", port: 8443, sub_id: "s1", groups: [] },
];

const LOGS = [
  { ts_ms: 1, text: "[info] kernel started" },
  { ts_ms: 2, text: "[warning] slow response" },
  { ts_ms: 3, text: "[error] port bind failed" },
  { ts_ms: 4, text: "[debug] tick" },
  { ts_ms: 5, text: "[trace] raw frame" },
  { ts_ms: 6, text: "uptime 3s" },
];

/**
 * 内核注册表夹具：sing-box 当前运行、xray 已装未选且缺 hysteria2（预检计数目标）、
 * mihomo 未装；restored/has_backup 双开以覆盖首屏横幅块。
 */
function statusDto(): ProxyStatusDto {
  return {
    mode: "system",
    kernel_running: true,
    kernel_id: "sing-box",
    inbound_port: 7890,
    nodes_total: 3,
    subs_total: 1,
    admin: true,
    wintun_installed: false,
    kernel_installed: true,
    kernel_version: "1.11.0",
    has_backup: true,
    restored_last_run: true,
    kernel: "sing-box",
    kernels: [
      {
        id: "sing-box",
        display_name: "sing-box",
        installed: true,
        version: "1.11.0",
        running: true,
        caps: { tun: true, policy_groups: true, external_controller: true },
        supported_kinds: ["shadowsocks", "vmess", "trojan", "vless", "hysteria2"],
      },
      {
        id: "xray",
        display_name: "xray-core",
        installed: true,
        version: "26.3.27",
        running: false,
        caps: { tun: false, policy_groups: false, external_controller: false },
        supported_kinds: ["shadowsocks", "vmess", "trojan", "vless"],
      },
      {
        id: "mihomo",
        display_name: "mihomo",
        installed: false,
        version: null,
        running: false,
        caps: { tun: true, policy_groups: true, external_controller: true },
        supported_kinds: ["shadowsocks", "vmess", "trojan", "vless", "hysteria2"],
      },
    ],
  };
}

let container: HTMLDivElement;
let root: Root;

const btnWithText = (scope: Element, text: string) =>
  Array.from(scope.querySelectorAll("button")).find((b) => b.textContent?.trim() === text);

function kernelRow(displayName: string): Element {
  const label = Array.from(container.querySelectorAll("span")).find(
    (el) => el.children.length === 0 && el.textContent?.trim() === displayName,
  );
  expect(label, `内核行标签 ${displayName} 未渲染`).toBeTruthy();
  let el: HTMLElement | null = (label as HTMLElement).parentElement;
  while (el && !btnWithText(el, "切换")) el = el.parentElement;
  expect(el, `${displayName} 行容器`).toBeTruthy();
  return el as Element;
}

async function mount() {
  vi.mocked(proxyStatus).mockResolvedValue(statusDto());
  await act(async () => {
    root = createRoot(container);
    root.render(<ProxyPanel />);
  });
  await act(async () => {});
}

async function goto(view: "overview" | "nodes" | "subs" | "rules" | "kernel" | "logs") {
  await act(async () => {
    useSession.getState().setProxySubPanel(view);
  });
  await act(async () => {});
}

async function click(el: Element) {
  await act(async () => {
    (el as HTMLElement).click();
  });
  await act(async () => {});
}

function setInput(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value")?.set;
  setter?.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  useSession.getState().setProxySubPanel("overview");
  useSession.getState().setThemeMode("auto");
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
  useSession.getState().setProxySubPanel("overview");
  vi.clearAllMocks();
  delete (navigator as { clipboard?: unknown }).clipboard;
});

describe("ProxyPanel 子面板化骨架（T-B2-3 六字面）", () => {
  it("proxySubnav_sixSectionsRender：六子面板各自挂载、旧 7 块内容迁移无丢失", async () => {
    await mount();
    // 旧 7 块 → 新归属逐一映射（每块取一枚只属于它的文案钉；首枚兼作其余视角的缺席判据。
    // rules 不用 textarea 内容做钉——React 把 value 设在属性上，textContent 不含它）
    const markers = {
      overview: ["运行模式", "检测到上次异常退出残留的系统代理", "存在原设置备份", "当前内核：sing-box", "换核与重启在「内核」子面板操作"],
      nodes: ["测速（TCP）", "HK-A", "TO-HY2"],
      subs: ["添加并拉取", "主订阅"],
      rules: ["保存规则", "命中即不走代理"],
      kernel: ["安装 wintun.dll（TUN 前置）", "内核按需下载", "xray-core"],
      logs: ["复制全部", "全部（6）"],
    } as const;
    const views = Object.keys(markers) as (keyof typeof markers)[];
    for (const view of views) {
      await goto(view);
      for (const text of markers[view]) {
        expect(container.textContent, `${view} 缺 ${text}`).toContain(text);
      }
      // 一次只渲染一个子面板：其余五块的代表文案必须缺席
      for (const other of views) {
        if (other === view) continue;
        expect(
          container.textContent,
          `${view} 视角不应看到 ${other} 的 ${markers[other][0]}`,
        ).not.toContain(markers[other][0]);
      }
    }
  });

  it("kernelCard_install_versionAndKernelArgs", async () => {
    await mount();
    await goto("kernel");
    const xr = kernelRow("xray-core");
    const input = xr.querySelector('input[placeholder="版本（可选）"]') as HTMLInputElement;
    expect(input).toBeTruthy();
    // 填版本 → 点名内核 + 版本透传
    await act(async () => {
      setInput(input, "26.3.27");
    });
    const statusCalls = vi.mocked(proxyStatus).mock.calls.length;
    await click(btnWithText(xr, "重新安装") as Element);
    expect(proxyKernelInstall).toHaveBeenCalledTimes(1);
    expect(proxyKernelInstall).toHaveBeenCalledWith("xray", "26.3.27");
    // 留空 → 任务书字面 undefined（前端不把"未填"伪造成 null，后端缺省=默认版本）
    await act(async () => {
      setInput(input, "");
    });
    await click(btnWithText(kernelRow("xray-core"), "重新安装") as Element);
    expect(proxyKernelInstall).toHaveBeenCalledTimes(2);
    expect(proxyKernelInstall).toHaveBeenLastCalledWith("xray", undefined);
    // 每次安装后重拉状态（installed/version 徽章以内端为真源）
    expect(vi.mocked(proxyStatus).mock.calls.length).toBe(statusCalls + 2);
  });

  it("kernelCard_switch_cancelZeroInvoke（红线负例）", async () => {
    await mount();
    await goto("kernel");
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    const statusCalls = vi.mocked(proxyStatus).mock.calls.length;
    await click(btnWithText(kernelRow("xray-core"), "切换") as Element);
    // 确认框按 danger+command 形态出发（红线删改类入口的 D-18 形状）
    expect(confirmAction).toHaveBeenCalledWith(
      expect.objectContaining({ danger: true, command: "xray", title: "切换内核" }),
    );
    expect(proxyKernelSelect).not.toHaveBeenCalled();
    // 取消连刷新都不许偷跑（零副作用）
    expect(vi.mocked(proxyStatus).mock.calls.length).toBe(statusCalls);
    expect(vi.mocked(proxyNodes).mock.calls.length).toBe(1);
  });

  it("kernelSwitchWizard_unsupportedCountReported：预检按 supported_kinds×现节点如实计数", async () => {
    await mount();
    await goto("kernel");
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await click(btnWithText(kernelRow("xray-core"), "切换") as Element);
    const opts = (vi.mocked(confirmAction).mock.calls[0] as [ConfirmOptions])[0];
    // impact 联合类型（string | string[]）走生产同款归一化，不猜形状
    const lines = impactLines(opts);
    expect(lines).toContain(
      "1 个节点该内核不支持（切换后这些节点无法作为出口）",
    );
    // hysteria2 只有一枚：计数不得虚增；"全部支持"兜底句同现即预检逻辑坏，也须缺席
    expect(lines.some((l) => l.includes("2 个节点"))).toBe(false);
    expect(lines.some((l) => l.includes("全部支持"))).toBe(false);
  });

  it("proxyLogs_levelFilter_frontendOnlyZeroInvoke", async () => {
    const writeText = vi.fn(async (..._args: unknown[]) => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    await mount();
    await goto("logs");
    expect(proxyLogs).toHaveBeenCalledTimes(1);
    const statusCalls = vi.mocked(proxyStatus).mock.calls.length;
    // error 芯片：只见 error 行，无级别行（uptime 3s）也被滤除
    await click(btnWithText(container, "error（1）") as Element);
    expect(container.textContent).toContain("[error] port bind failed");
    expect(container.textContent).not.toContain("[info] kernel started");
    expect(container.textContent).not.toContain("uptime 3s");
    // 切回全部：无级别行只在「全部」出现（不猜级别）
    await click(btnWithText(container, "全部（6）") as Element);
    expect(container.textContent).toContain("uptime 3s");
    // 复制全部走 clipboard（本地行为，同样零 invoke）
    await click(btnWithText(container, "复制全部") as Element);
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith(LOGS.map((l) => l.text).join("\n"));
    // 清空=只清本地列表：后端环形缓冲不接指令
    await click(btnWithText(container, "清空") as Element);
    expect(container.textContent).toContain("暂无日志（内核未启动或未产生输出）");
    expect(container.textContent).toContain("全部（0）");
    // 红线：整个过滤/复制/清空过程零新 invoke
    expect(proxyLogs).toHaveBeenCalledTimes(1);
    expect(vi.mocked(proxyStatus).mock.calls.length).toBe(statusCalls);
  });

  it("proxySubPanel_selectionPersistedRoundtrip：partialize 落盘含键、旧快照缺键回退 overview", async () => {
    useSession.getState().setProxySubPanel("kernel");
    const raw = JSON.parse(localStorage.getItem("nf-session") ?? "{}") as {
      state?: Record<string, unknown>;
    };
    expect(raw.state?.proxySubPanel).toBe("kernel");
    expect(raw.state).toHaveProperty("themeMode");
    // 野值不落 store（持久化快照被手改也不换面板）
    useSession.getState().setProxySubPanel("bogus");
    expect(useSession.getState().proxySubPanel).toBe("kernel");
    // 旧快照（无 proxySubPanel 键）= 模拟升级后首启：回退初始 overview。
    // 先复位内存再播种（jsdom setItem 同步派发 storage → 末端链只读旧快照），
    // themeMode 阳性对照证明确实发生了水合而非静默保持默认。
    useSession.getState().setProxySubPanel("overview");
    const legacy = JSON.stringify({
      state: { themeMode: "light", activeModule: "clipboard", clipGroup: "all" },
      version: 0,
    });
    localStorage.setItem("nf-session", legacy);
    window.dispatchEvent(
      new StorageEvent("storage", { key: "nf-session", newValue: legacy }),
    );
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
    expect(useSession.getState().themeMode).toBe("light");
    expect(useSession.getState().proxySubPanel).toBe("overview");
  });
});
