import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ProxyPanel from "../ProxyPanel";
import {
  proxyArtifactInstall,
  proxyKernelSelect,
  proxyStatus,
  type ProxyArtifactInfoDto,
  type ProxyStatusDto,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";
import { useSession } from "../../../stores/session";

// D-29 B2/T-B2-3：T-B2-2 的状态徽章区最小 Dropdown 选择器已被「内核」子面板的
// 完整内核卡取代（09 §5.2 内核卡行字面）。四枚测试名逐字保留，驱动方式改为
// 内核卡行的「切换」按钮；Dropdown 桩就此删除——代理面板已无 Fluent Dropdown。
// 换核回滚语义仍全在后端，UI 只经 run() 错误通道如实呈现上抛原错。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    proxyStatus: vi.fn(),
    proxySubs: vi.fn(async () => []),
    proxyNodes: vi.fn(async () => []),
    proxyRulesGet: vi.fn(async () => ({ rules: [], final_target: "proxy", route_mode: "rule" })),
    proxyLogs: vi.fn(async () => []),
    proxyKernelSelect: vi.fn(async () => {}),
    proxyKernelInstall: vi.fn(async () => ({})),
    proxyKernelRestart: vi.fn(async () => {}),
    proxyWintunInstall: vi.fn(async () => {}),
    proxyArtifactInstall: vi.fn(async () => ({})),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

/**
 * 三内核注册表夹具（UI 只按数据渲染，禁内核特例分支）：
 * sing-box = 当前且运行中；xray-core = 已装未选（合法切换目标）；mihomo = 未装。
 */
function statusDto(overrides: Partial<ProxyStatusDto> = {}): ProxyStatusDto {
  return {
    mode: "system",
    kernel_running: true,
    kernel_id: "sing-box",
    inbound_port: 7890,
    nodes_total: 0,
    subs_total: 0,
    admin: true,
    wintun_installed: false,
    kernel_installed: true,
    kernel_version: "1.11.0",
    has_backup: false,
    restored_last_run: false,
    kernel: "sing-box",
    kernels: [
      {
        id: "sing-box",
        display_name: "sing-box",
        installed: true,
        version: "1.11.0",
        running: true,
        caps: { tun: true, policy_groups: true, external_controller: true },
        supported_kinds: ["shadowsocks", "vmess", "trojan", "vless"],
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
    artifacts: [],
    selected_node: null,
    selected_stale: false,
    ...overrides,
  };
}

let container: HTMLDivElement;
let root: Root;

const btnWithText = (scope: Element, text: string) =>
  Array.from(scope.querySelectorAll("button")).find((b) => b.textContent?.trim() === text);

/** 从内核 display_name 标签上溯到含「切换」按钮的最近祖先行容器（= 内核卡行 div） */
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

async function mount(status: ProxyStatusDto) {
  vi.mocked(proxyStatus).mockResolvedValue(status);
  await act(async () => {
    root = createRoot(container);
    root.render(<ProxyPanel />);
  });
  await act(async () => {});
}

async function gotoKernel() {
  await act(async () => {
    useSession.getState().setProxySubPanel("kernel");
  });
  await act(async () => {});
}

async function click(el: Element) {
  await act(async () => {
    (el as HTMLElement).click();
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  useSession.getState().setProxySubPanel("overview");
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
});

describe("ProxyPanel 内核卡换核接线（T-B2-3 接管 T-B2-2 四测）", () => {
  it("kernelSelect_statusBadge_listsRegistryAndMarksCurrent", async () => {
    await mount(statusDto());
    await gotoKernel();
    const sb = kernelRow("sing-box");
    const xr = kernelRow("xray-core");
    const mh = kernelRow("mihomo");
    // 当前核 + 运行中标记；其余行不得沾（禁前端猜，全按注册表数据渲染）
    expect(sb.textContent).toContain("当前");
    expect(sb.textContent).toContain("运行中");
    expect(sb.textContent).toContain("v1.11.0");
    expect(xr.textContent).not.toContain("当前");
    expect(xr.textContent).not.toContain("运行中");
    expect(mh.textContent).toContain("未安装");
    // 能力行如实（TUN 门禁的数据源）
    expect(xr.textContent).toContain("TUN 不支持");
    expect(mh.textContent).toContain("TUN 支持");
    // 纯渲染零副作用：没点就不许碰后端
    expect(proxyKernelSelect).not.toHaveBeenCalled();
  });

  it("kernelSelect_differentOption_invokesOnceAndRefreshes", async () => {
    await mount(statusDto());
    await gotoKernel();
    const statusCallsAfterMount = vi.mocked(proxyStatus).mock.calls.length;
    const switchBtn = btnWithText(kernelRow("xray-core"), "切换");
    expect(switchBtn).toBeTruthy();
    await click(switchBtn as Element);
    expect(proxyKernelSelect).toHaveBeenCalledTimes(1);
    expect(proxyKernelSelect).toHaveBeenCalledWith("xray");
    // 换核后必须重拉状态（选择/运行标记以内端为真源，UI 不本地改写）
    expect(vi.mocked(proxyStatus).mock.calls.length).toBe(statusCallsAfterMount + 1);
    // busy 落定后按钮重新可用
    expect((btnWithText(kernelRow("xray-core"), "切换") as HTMLButtonElement).disabled).toBe(
      false,
    );
  });

  it("kernelSelect_sameCurrentIdempotent：同核与未装核双双零 invoke（负例）", async () => {
    await mount(statusDto());
    await gotoKernel();
    const statusCalls = vi.mocked(proxyStatus).mock.calls.length;
    // 两枚门禁都以属性钉死（jsdom 会对 disabled 按钮派发 click，处理器防御臂仍在）
    const same = btnWithText(kernelRow("sing-box"), "切换") as HTMLButtonElement;
    const notInstalled = btnWithText(kernelRow("mihomo"), "切换") as HTMLButtonElement;
    expect(same.disabled).toBe(true);
    expect(same.title).toBe("已是当前内核");
    expect(notInstalled.disabled).toBe(true);
    expect(notInstalled.title).toBe("该内核未安装：先安装后切换");
    await click(same);
    await click(notInstalled);
    expect(proxyKernelSelect).not.toHaveBeenCalled();
    expect(vi.mocked(proxyStatus).mock.calls.length).toBe(statusCalls);
  });

  it("kernelSelect_failureLandsInErrorChannel：后端回滚上抛原错如实可见", async () => {
    await mount(statusDto());
    await gotoKernel();
    vi.mocked(proxyKernelSelect).mockRejectedValueOnce(
      new Error("切换内核 xray 失败：测试内核启动失败；回滚原内核成功"),
    );
    await click(btnWithText(kernelRow("xray-core"), "切换") as Element);
    expect(proxyKernelSelect).toHaveBeenCalledTimes(1);
    // run() 错误通道：InlineError 呈现原错文本，且不得静默吞掉
    expect(container.textContent).toContain("回滚原内核成功");
    expect(
      (btnWithText(kernelRow("xray-core"), "切换") as HTMLButtonElement).disabled,
    ).toBe(false);
  });
});

describe("ProxyPanel geo 数据资产行（T-B2-10）", () => {
  const ARTIFACTS: ProxyArtifactInfoDto[] = [
    { id: "singbox-geosite", label: "sing-box GeoSite 码表", installed: false, version: null },
    { id: "singbox-geoip", label: "sing-box GeoIP 码表", installed: true, version: "20260912" },
  ];

  function artifactRow(label: string): Element {
    const label0 = Array.from(container.querySelectorAll("span")).find(
      (el) => el.children.length === 0 && el.textContent?.trim() === label,
    );
    expect(label0, `资产行标签 ${label} 未渲染`).toBeTruthy();
    return (label0 as HTMLElement).parentElement as Element;
  }

  it("geoArtifact_installButton_invokes：确认一次 invoke，取消零 invoke（负例）", async () => {
    await mount(statusDto({ artifacts: ARTIFACTS }));
    await gotoKernel();
    expect(artifactRow("sing-box GeoSite 码表").textContent).toContain("未安装");
    expect(artifactRow("sing-box GeoIP 码表").textContent).toContain("v20260912");
    await click(btnWithText(artifactRow("sing-box GeoSite 码表"), "安装") as Element);
    expect(proxyArtifactInstall).toHaveBeenCalledTimes(1);
    expect(proxyArtifactInstall).toHaveBeenCalledWith("singbox-geosite");
    // 取消确认＝零 invoke：重装钮在场但确认后不触后端
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await click(btnWithText(artifactRow("sing-box GeoIP 码表"), "重新安装") as Element);
    expect(proxyArtifactInstall).toHaveBeenCalledTimes(1);
  });
});
