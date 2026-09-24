import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";

import FilePanel from "../FilePanel";
import RemoteBrowser from "../RemoteBrowser";
import { parse_magic_target } from "../magicTarget";
import {
  fileBreadcrumbs,
  fileDrives,
  fileEnqueue,
  fileList,
  fileOpsActive,
  fileOpsPending,
  fileRemoteBrowse,
  fileRemoteDrivers,
  fileRemotePresets,
  fileRemoteProfiles,
  type FileEntryDto,
  type RemoteDriverDto,
  type RemoteEntryDto,
  type RemotePresetDto,
  type RemoteProfileDto,
} from "../../../ipc/client";
import { notify } from "../../../stores/notifications";
import { useSession } from "../../../stores/session";

// D-29 B6/T-B6-11：magicTarget.ts 的消费面落地（T-B6-10 补记⑥的挂账）——
// 魔术栏分派只走 parse_magic_target 一处；远端浏览的下载/上传钮把 OpEndpoint
// 投进 fileEnqueue（队列远端执行器已接线，"不摆假钮"的登记翻正）。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    fileList: vi.fn(),
    fileBreadcrumbs: vi.fn(),
    fileDrives: vi.fn(),
    fileEnqueue: vi.fn(),
    fileOpsActive: vi.fn(),
    fileOpsPending: vi.fn(),
    fileRemoteBrowse: vi.fn(),
    fileRemoteDrivers: vi.fn(),
    fileRemotePresets: vi.fn(),
    fileRemoteProfiles: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

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

function remoteEntry(name: string, isDir: boolean): RemoteEntryDto {
  return {
    name,
    path: `/docs/${name}`,
    is_dir: isDir,
    size: isDir ? 0 : 4096,
    modified_ms: Date.parse("2026-09-19T10:00:00"),
  };
}

function driverDto(): RemoteDriverDto {
  return {
    driver_id: "remote:ps1",
    label: "归档站",
    protocol: "sftp",
    host: "files.example.com",
    port: 22,
    base_path: "/docs",
    roots: ["/docs"],
    auth_source: "anonymous",
  };
}

function profileDto(over: Partial<RemoteProfileDto> = {}): RemoteProfileDto {
  return {
    id: "remote:ps1",
    label: "归档站",
    protocol: "sftp",
    host: "files.example.com",
    port: 22,
    user: "me",
    base_path: "/docs",
    auth: { kind: "anonymous" },
    preset_id: null,
    last_used_ms: 0,
    ...over,
  };
}

function presetDto(over: Partial<RemotePresetDto> = {}): RemotePresetDto {
  return {
    id: "p-sftp",
    label: "SFTP 站",
    protocol: "sftp",
    default_host: "files.example.com",
    port: 22,
    base_path: "/srv",
    auth_kind: "anonymous",
    notes: "",
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
}
function buttonByContains(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(text),
  );
}
function rowByText(text: string): Element | undefined {
  return [...container.querySelectorAll("tr")].find((r) => r.textContent?.includes(text));
}
function inputByPlaceholder(ph: string): HTMLInputElement {
  return document.querySelector<HTMLInputElement>(`input[placeholder="${ph}"]`)!;
}
function inputByAria(label: string): HTMLInputElement | null {
  // Fluent Input 的 aria-label 挂在内部 <input> 上
  return document.querySelector<HTMLInputElement>(`input[aria-label="${label}"]`);
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function setInput(el: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function mount(node: ReactNode) {
  await act(async () => {
    root = createRoot(container);
    root.render(node);
  });
  await act(async () => {});
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(fileList).mockResolvedValue([entry("a.txt", false)]);
  vi.mocked(fileBreadcrumbs).mockResolvedValue([["C:", "C:\\"]]);
  vi.mocked(fileDrives).mockResolvedValue([]);
  vi.mocked(fileOpsActive).mockResolvedValue([]);
  vi.mocked(fileOpsPending).mockResolvedValue([]);
  vi.mocked(fileEnqueue).mockResolvedValue({ op_id: "op-x", conflicts: [], name_fix: [] });
  vi.mocked(fileRemotePresets).mockResolvedValue([]);
  vi.mocked(fileRemoteProfiles).mockResolvedValue([profileDto()]);
  vi.mocked(fileRemoteDrivers).mockResolvedValue([driverDto()]);
  vi.mocked(fileRemoteBrowse).mockResolvedValue([remoteEntry("a.bin", false)]);
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
  useSession.setState({ fileSubPanel: "browse" });
  vi.clearAllMocks();
});

describe("magicTarget 纯函数（解析分派唯一算式）", () => {
  it("magicTarget_pure_allArms", () => {
    // 盘符三种形态
    expect(parse_magic_target("C:", [])).toEqual({ kind: "drive", value: "C:" });
    expect(parse_magic_target("C:\\a\\b", [])).toEqual({ kind: "drive", value: "C:\\a\\b" });
    expect(parse_magic_target("D:/x", [])).toEqual({ kind: "drive", value: "D:/x" });
    // scheme 归一：webdav 惯写名 → serde 规范名 web_dav
    expect(parse_magic_target("webdav://h/d", [])).toEqual({
      kind: "remote",
      value: "web_dav://h/d",
    });
    expect(parse_magic_target("sftp://h", [])).toEqual({ kind: "remote", value: "sftp://h" });
    // 未知 scheme 不猜（这是被禁的"当远端主机名试试"兜底的反面）
    expect(parse_magic_target("gopher://x/y", [])).toBeNull();
    expect(parse_magic_target("", [])).toBeNull();
    expect(parse_magic_target("  ", [])).toBeNull();
    expect(parse_magic_target("C:\\dir\\a.txt", [])).toEqual({
      kind: "drive",
      value: "C:\\dir\\a.txt",
    });
    // 预设主机直呼（精确 + 前缀带子路径）
    const presets = [presetDto()];
    expect(parse_magic_target("files.example.com/pub", presets)).toEqual({
      kind: "remote",
      value: "sftp://files.example.com/pub",
    });
    expect(parse_magic_target("files.example.com", presets)).toEqual({
      kind: "remote",
      value: "sftp://files.example.com",
    });
    // 无预设时裸主机名不猜
    expect(parse_magic_target("files.example.com/pub", [])).toBeNull();
  });
});

describe("FilePanel 魔术栏消费（T-B6-11 挂账清偿）", () => {
  async function copySelected(dst: string) {
    await mount(<FilePanel />);
    await click(rowByText("a.txt")!);
    await setInput(inputByPlaceholder("目标目录（复制/移动用）"), dst);
    await click(buttonByContains("复制到")!);
  }

  it("magicBar_remoteTarget_enqueuesRemoteEndpointOnceConnected", async () => {
    await copySelected("sftp://files.example.com/pub");
    expect(fileEnqueue).toHaveBeenCalledTimes(1);
    const spec = vi.mocked(fileEnqueue).mock.calls[0][0];
    expect(spec.dst).toEqual({ driver_id: "remote:ps1", path: "/pub" });
    expect(spec.srcs).toEqual(["C:\\dir\\a.txt"]);
    expect(spec.kind).toBe("copy");
  });

  it("magicBar_remoteTarget_notConnected_namesReconnect", async () => {
    vi.mocked(fileRemoteDrivers).mockResolvedValue([]);
    await copySelected("sftp://files.example.com/pub");
    expect(fileEnqueue).not.toHaveBeenCalled();
    expect(container.textContent).toContain("未连接");
    // 无事实源就不承诺：也不得回落成字符串 dst 入队
    expect(JSON.stringify(vi.mocked(fileEnqueue).mock.calls)).not.toContain("driver_id");
  });

  it("magicBar_unknownScheme_refusesAsNotAddress", async () => {
    await copySelected("gopher://files.example.com/x");
    expect(fileEnqueue).not.toHaveBeenCalled();
    expect(container.textContent).toContain("无法识别为路径或连接地址");
  });

  it("magicBar_driveTarget_keepsLegacyStringDst_positiveControl", async () => {
    await copySelected("D:\\bak");
    expect(fileEnqueue).toHaveBeenCalledTimes(1);
    expect(vi.mocked(fileEnqueue).mock.calls[0][0].dst).toBe("D:\\bak");
  });

  it("magicBar_hostWithoutProfile_namesMissingProfile", async () => {
    vi.mocked(fileRemoteProfiles).mockResolvedValue([]);
    await copySelected("sftp://elsewhere.example.org/pub");
    expect(fileEnqueue).not.toHaveBeenCalled();
    expect(container.textContent).toContain("没有建档");
  });
});

describe("RemoteBrowser 传输投递钮（不摆假钮的翻正）", () => {
  it("remoteBrowser_downloadEnqueuesRemoteEndpoint", async () => {
    await mount(<RemoteBrowser />);
    await click(buttonByText("浏览")!); // /docs 已由初值带出
    await setInput(inputByAria("本地落点目录")!, "D:\\down");
    await click(buttonByText("下载")!);
    expect(fileEnqueue).toHaveBeenCalledTimes(1);
    const spec = vi.mocked(fileEnqueue).mock.calls[0][0];
    expect(spec.kind).toBe("copy");
    expect(spec.srcs).toEqual([{ driver_id: "remote:ps1", path: "/docs/a.bin" }]);
    expect(spec.dst).toBe("D:\\down\\a.bin");
    expect(notify).toHaveBeenCalledWith("success", "下载已入队", expect.any(String));
  });

  it("remoteBrowser_downloadWithoutDst_refusesAndNamesNoGuess", async () => {
    await mount(<RemoteBrowser />);
    await click(buttonByText("浏览")!);
    await click(buttonByText("下载")!);
    expect(fileEnqueue).not.toHaveBeenCalled();
    expect(container.textContent).toContain("不代猜");
  });

  it("remoteBrowser_uploadEnqueuesLocalSrcAgainstCurrentDir", async () => {
    await mount(<RemoteBrowser />);
    await click(buttonByText("浏览")!);
    await setInput(inputByAria("本地上传源")!, "C:\\src\\big.bin");
    await click(buttonByText("上传到此目录")!);
    expect(fileEnqueue).toHaveBeenCalledTimes(1);
    const spec = vi.mocked(fileEnqueue).mock.calls[0][0];
    expect(spec.srcs).toEqual(["C:\\src\\big.bin"]);
    expect(spec.dst).toEqual({ driver_id: "remote:ps1", path: "/docs/big.bin" });
    expect(notify).toHaveBeenCalledWith("success", "上传已入队", expect.any(String));
  });

  it("remoteBrowser_enqueueRejectsHonestConflictList", async () => {
    vi.mocked(fileEnqueue).mockResolvedValue({
      op_id: null,
      conflicts: [{ name: "a.bin", dst: "/docs/a.bin" }],
      name_fix: [],
    });
    await mount(<RemoteBrowser />);
    await click(buttonByText("浏览")!);
    await setInput(inputByAria("本地落点目录")!, "D:\\down");
    await click(buttonByText("下载")!);
    // 远端源跳过了本地预扫描仍被服务端闸拦下：面板如实转述，不谎称已入队
    expect(notify).not.toHaveBeenCalled();
    expect(container.textContent).toContain("未决议的同名冲突");
  });
});
