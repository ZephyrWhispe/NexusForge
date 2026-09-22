import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ClipboardPanel from "../ClipboardPanel";
import {
  clipboardCaptureGet,
  clipboardGet,
  clipboardGroupCounts,
  clipboardSearch,
  clipboardSecretReveal,
  clipboardStats,
  hostConfigGet,
  hostConfigSchema,
  hostConfigSet,
  type ClipEntry,
} from "../../../ipc/client";
import { confirmAction, impactLines } from "../../../stores/confirm";
import { notify } from "../../../stores/notifications";
import { useSession, type ClipView } from "../../../stores/session";
import SecretSection, { toLiteralRegex } from "../panels/SecretSection";

// D-29 B3/T-B3-7 回归（09 §8.2）：内容屏蔽规则的两个写口都只走 host_config_set，
// 且必须是**读-改-写**——该命令是整份替换语义，漏带任一键就等于把它静默写成缺省值。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    clipboardSearch: vi.fn(),
    clipboardGet: vi.fn(),
    clipboardPaste: vi.fn(),
    clipboardPin: vi.fn(),
    clipboardDelete: vi.fn(),
    clipboardGroupCounts: vi.fn(),
    clipboardCaptureGet: vi.fn(),
    clipboardSecretReveal: vi.fn(),
    clipboardStats: vi.fn(),
    hostConfigSchema: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
  };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

// jsdom 无 ResizeObserver/真实 rect，虚拟器观测不到滚动容器就不产行（同 secretReveal.test 处置）
vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 64,
    getVirtualItems: () =>
      Array.from({ length: count }, (_, i) => ({ index: i, start: i * 64, key: i, size: 64 })),
    measureElement: () => {},
  }),
}));

/** 盘上真源全量：九键一份不缺，测试据此核对"读-改-写"没丢键 */
const CFG = {
  max_entries: 5000,
  retention_days: 30,
  capture_images: true,
  capture_files: true,
  sensitive_filter: true,
  auto_group: true,
  excluded_apps: ["1password"],
  capture_paused: false,
  block_patterns: ["\\d{6}$"],
};
const CFG_KEYS = Object.keys(CFG).sort();
const MASK = "[敏感内容] 已加密存储";

function clip(id: string, over: Partial<ClipEntry> = {}): ClipEntry {
  return {
    id,
    content_type: "text",
    preview: MASK,
    blob_path: null,
    origin: "local",
    source_app: "Browser",
    pinned: false,
    group: null,
    secret: true,
    has_html: false,
    created_at: Date.parse("2026-09-22T09:10:00"),
    usage_count: 0,
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(clipboardSearch).mockResolvedValue({
    items: [clip("sec1")],
    has_more: false,
    total: null,
  });
  vi.mocked(clipboardGroupCounts).mockResolvedValue({ secret: 1 });
  vi.mocked(clipboardCaptureGet).mockResolvedValue({ paused: false, skipped: 0 });
  vi.mocked(clipboardStats).mockResolvedValue({
    total: 1,
    by_content_type: { text: 1 },
    by_group: {},
    top_source_apps: [],
    bytes_blob: 0,
  });
  vi.mocked(clipboardSecretReveal).mockResolvedValue({ id: "sec1", text: "明文不外泄" });
  vi.mocked(hostConfigSchema).mockResolvedValue({ properties: {} });
  vi.mocked(hostConfigGet).mockResolvedValue({ ...CFG });
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => {
    try {
      root?.unmount();
    } catch {
      /* 用例内已卸载 */
    }
  });
  root = null;
  container.remove();
  vi.clearAllMocks();
  useSession.setState({ clipView: "history", clipGroup: "all" });
});

async function mount(view: ClipView) {
  act(() => useSession.getState().setClipView(view));
  await act(async () => {
    root = createRoot(container);
    root.render(<ClipboardPanel search="" group="all" onCounts={() => {}} />);
  });
  await act(async () => {});
}

async function click(el: Element | undefined | null) {
  if (!el) throw new Error("目标控件未渲染");
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

const buttonByText = (text: string) =>
  [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);

/** 原生 setter 赋值：React 受控 Textarea 只认这条路径 */
async function typeInto(label: string, value: string) {
  const box = ruleBox(label);
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")?.set;
  await act(async () => {
    setter!.call(box, value);
    box.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await act(async () => {});
}

const ruleBox = (label = "内容屏蔽规则") => {
  const box = container.querySelector<HTMLTextAreaElement>(`[aria-label="${label}"]`);
  if (!box) throw new Error(`输入区 ${label} 未渲染`);
  return box;
};

const savedPayload = () => vi.mocked(hostConfigSet).mock.calls[0]?.[1] as Record<string, unknown>;

describe("内容屏蔽规则写口（T-B3-7）", () => {
  it("captureBlockRules_editSavesViaHostConfigSet_mergedKeys：一行一条编辑后整份合并写回，既有八键一键不丢", async () => {
    await mount("settings");
    // 盘上既有规则原样回填（不是空表，否则用户以为从没配过）——须在编辑前读
    expect(ruleBox().value).toBe("\\d{6}$");

    await typeInto("内容屏蔽规则", "\\d{6}$\n\n  ^(sk-|ghp_)  \n");
    await click(buttonByText("保存规则"));
    expect(hostConfigSet).toHaveBeenCalledTimes(1);
    expect(vi.mocked(hostConfigSet).mock.calls[0]?.[0]).toBe("clipboard");
    const payload = savedPayload();
    // 红线：九键全集，缺任一键即被 host_config_set 的整份替换语义写成缺省值
    expect(Object.keys(payload).sort()).toEqual(CFG_KEYS);
    expect(payload.max_entries).toBe(5000);
    expect(payload.retention_days).toBe(30);
    expect(payload.capture_images).toBe(true);
    expect(payload.capture_files).toBe(true);
    expect(payload.sensitive_filter).toBe(true);
    expect(payload.auto_group).toBe(true);
    expect(payload.excluded_apps).toEqual(["1password"]);
    expect(payload.capture_paused).toBe(false);
    // 空行丢弃、首尾空白剥掉、顺序保持（规则按序短路）
    expect(payload.block_patterns).toEqual(["\\d{6}$", "^(sk-|ghp_)"]);
    expect(notify).toHaveBeenCalledWith(
      "success",
      "已保存内容屏蔽规则",
      expect.stringContaining("仅文本捕获"),
    );
  });

  it("captureBlockRules_secondStoredPattern_saveKeepsOrder：换一份盘上夹具再走一遍，顺序与键集合同样成立", async () => {
    vi.mocked(hostConfigGet).mockResolvedValue({ ...CFG, block_patterns: ["^token-"] });
    await mount("settings");
    expect(ruleBox().value).toBe("^token-");
    await typeInto("内容屏蔽规则", "^token-\n第二行");
    await click(buttonByText("保存规则"));
    expect(savedPayload().block_patterns).toEqual(["^token-", "第二行"]);
    expect(Object.keys(savedPayload()).sort()).toEqual(CFG_KEYS);
  });

  it("neverStoreButton_generatesLiteralPatternAndConfirms：确认弹窗展示定串规则，确认后合并写回", async () => {
    // 跨语言契约：本期望字面串与 clipboard-core query.rs 的 secretRow_neverStore_… 同源
    expect(toLiteralRegex("sk-abc.1(key)|x")).toBe("sk-abc\\.1\\(key\\)\\|x");
    expect(toLiteralRegex("验证码 123456")).toBe("验证码 123456");

    await mount("secret");
    await click(buttonByText("永不入库"));
    const arg = vi.mocked(confirmAction).mock.calls[0]?.[0];
    expect(arg?.danger).toBe(true);
    const shown = [...impactLines(arg ?? {}), arg?.detail ?? ""].join("\n");
    expect(shown).toContain(toLiteralRegex(MASK));
    // 诚实边界：本行是掩码预览，规则匹配的是掩码文案而非明文，弹窗须说白
    expect(shown).toContain("掩码");
    expect(shown).not.toContain("明文不外泄");

    expect(hostConfigGet).toHaveBeenCalledWith("clipboard");
    const payload = savedPayload();
    expect(Object.keys(payload).sort()).toEqual(CFG_KEYS);
    expect(payload.block_patterns).toEqual(["\\d{6}$", toLiteralRegex(MASK)]);
    // 敏感库唯一的读明文口不因本操作被绕开
    expect(clipboardSecretReveal).not.toHaveBeenCalled();
  });

  it("neverStoreButton_cancelZeroInvoke：确认取消即零配置读写，屏蔽表不动", async () => {
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await mount("secret");
    await click(buttonByText("永不入库"));
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(hostConfigGet).not.toHaveBeenCalled();
    expect(hostConfigSet).not.toHaveBeenCalled();
    expect(clipboardGet).not.toHaveBeenCalled();
    // 行仍在原处（取消不改变列表）
    expect(container.textContent).toContain(MASK);
  });

  it("neverStoreButton_existingPatternSkipsWrite：同一条规则不重复追加", async () => {
    vi.mocked(hostConfigGet).mockResolvedValue({
      ...CFG,
      block_patterns: ["\\d{6}$", toLiteralRegex(MASK)],
    });
    await act(async () => {
      root = createRoot(container);
      root.render(<SecretSection />);
    });
    await act(async () => {});
    await click(buttonByText("永不入库"));
    expect(hostConfigSet).not.toHaveBeenCalled();
    expect(notify).toHaveBeenCalledWith("info", expect.any(String), toLiteralRegex(MASK));
  });
});
