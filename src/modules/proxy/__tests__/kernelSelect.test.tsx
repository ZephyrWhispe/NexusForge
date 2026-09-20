import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactNode } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ProxyPanel from "../ProxyPanel";
import { proxyKernelSelect, proxyStatus, type ProxyStatusDto } from "../../../ipc/client";

// D-29 B2/T-B2-2 回归：状态徽章区最小内核选择器接线 ——
// optionValue → proxyKernelSelect → refresh 单链、同内核幂等负例、busy 门与
// 失败经 run() 错误通道如实落 InlineError（换核回滚语义在后端，UI 不猜中间态）。
//
// 桩边界：@fluentui/react-components 的 Dropdown 弹出/定位属 Fluent 自身测试面，
// jsdom 下不可控；此处只替换 Dropdown/Option 为记录 props 的桩，其余组件
// （Badge/Button/Table/makeStyles…）保持真实渲染。
type DropdownStubProps = {
  value?: string;
  disabled?: boolean;
  children?: ReactNode;
  onOptionSelect?: (e: unknown, data: { optionValue?: string }) => void;
};

const harness = vi.hoisted(() => ({
  dropdownProps: null as DropdownStubProps | null,
}));

vi.mock("@fluentui/react-components", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@fluentui/react-components")>();
  return {
    ...actual,
    Dropdown: (props: DropdownStubProps) => {
      harness.dropdownProps = props;
      return (
        <div data-testid="kernel-dropdown">
          <span data-testid="kernel-dropdown-value">{props.value}</span>
          {props.children}
        </div>
      );
    },
    Option: ({ children }: { children?: ReactNode }) => <span>{children}</span>,
  };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    proxyStatus: vi.fn(),
    proxySubs: vi.fn(async () => []),
    proxyNodes: vi.fn(async () => []),
    proxyDirectRules: vi.fn(async () => []),
    proxyLogs: vi.fn(async () => []),
    proxyKernelSelect: vi.fn(async () => {}),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

/** 双内核注册表夹具：UI 只按数据渲染列表（今天真机注册表只 1 项不构成分支差异） */
function statusDto(overrides: Partial<ProxyStatusDto> = {}): ProxyStatusDto {
  return {
    mode: "off",
    kernel_running: false,
    kernel_id: null,
    inbound_port: 7890,
    nodes_total: 1,
    subs_total: 0,
    admin: false,
    wintun_installed: false,
    kernel_installed: true,
    kernel_version: "1.10.0",
    has_backup: false,
    restored_last_run: false,
    kernel: "sing-box",
    kernels: [
      {
        id: "sing-box",
        display_name: "sing-box",
        installed: true,
        version: "1.10.0",
        running: false,
        caps: { tun: true, policy_groups: true, external_controller: true },
        supported_kinds: ["shadowsocks", "vmess", "trojan", "vless"],
      },
      {
        id: "xray",
        display_name: "xray-core",
        installed: false,
        version: null,
        running: false,
        caps: { tun: false, policy_groups: false, external_controller: false },
        supported_kinds: ["shadowsocks", "vmess", "trojan", "vless"],
      },
    ],
    ...overrides,
  };
}

let container: HTMLDivElement;
let root: Root;

async function mount(status: ProxyStatusDto) {
  vi.mocked(proxyStatus).mockResolvedValue(status);
  await act(async () => {
    root = createRoot(container);
    root.render(<ProxyPanel />);
  });
  await act(async () => {});
}

async function selectOption(optionValue: string) {
  await act(async () => {
    harness.dropdownProps?.onOptionSelect?.({}, { optionValue });
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  harness.dropdownProps = null;
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

describe("ProxyPanel 内核选择器（T-B2-2 状态徽章区）", () => {
  it("kernelSelect_statusBadge_listsRegistryAndMarksCurrent", async () => {
    await mount(statusDto());
    expect(harness.dropdownProps).not.toBeNull();
    expect(harness.dropdownProps?.value).toContain("内核：sing-box");
    // 当前核高亮 + 未装核如实标注（禁前端内核特例分支，全靠注册表数据）
    expect(container.textContent).toContain("sing-box（当前）");
    expect(container.textContent).toContain("xray-core（未安装）");
    // 纯渲染零副作用：没选就不许碰后端
    expect(proxyKernelSelect).not.toHaveBeenCalled();
  });

  it("kernelSelect_differentOption_invokesOnceAndRefreshes", async () => {
    await mount(statusDto());
    const statusCallsAfterMount = vi.mocked(proxyStatus).mock.calls.length;
    await selectOption("xray");
    expect(proxyKernelSelect).toHaveBeenCalledTimes(1);
    expect(proxyKernelSelect).toHaveBeenCalledWith("xray");
    // 换核后必须重拉状态（选择/运行标记以内端为真源，UI 不本地改写）
    expect(vi.mocked(proxyStatus).mock.calls.length).toBe(statusCallsAfterMount + 1);
    // busy 落定后选择器重新可用
    expect(harness.dropdownProps?.disabled).toBe(false);
  });

  it("kernelSelect_sameCurrentIdempotent：同核与空值双双零 invoke（负例）", async () => {
    await mount(statusDto());
    const statusCalls = vi.mocked(proxyStatus).mock.calls.length;
    await selectOption("sing-box"); // 已是当前核
    await selectOption(""); // 空 optionValue 防御臂
    expect(proxyKernelSelect).not.toHaveBeenCalled();
    expect(vi.mocked(proxyStatus).mock.calls.length).toBe(statusCalls);
  });

  it("kernelSelect_failureLandsInErrorChannel：后端回滚上抛原错如实可见", async () => {
    await mount(statusDto());
    vi.mocked(proxyKernelSelect).mockRejectedValueOnce(
      new Error("切换内核 xray 失败：测试内核启动失败；回滚原内核成功"),
    );
    await selectOption("xray");
    expect(proxyKernelSelect).toHaveBeenCalledTimes(1);
    // run() 错误通道：InlineError 呈现原错文本，且不得静默吞掉
    expect(container.textContent).toContain("回滚原内核成功");
    expect(harness.dropdownProps?.disabled).toBe(false);
  });
});
