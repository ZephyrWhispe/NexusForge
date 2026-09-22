/**
 * 完成后动作链配置化（09 §9.2 T-B4-8）：面板预设写 `post_actions` + 覆盖层「完成」钮读
 * `TaskStartDto.default_actions`。
 *
 * §9.1-④ 两半制沿用：动作名单的语义住纯模块（`src/windows/overlay/postActions.ts`，
 * 本文件直接调用），装配面（OverlayShot 装载不了 jsdom——它要真 canvas 2d 上下文与
 * Tauri 窗口 API）以 `?raw` 源码扫描钉住调用形状。面板那一半是真挂载：点预设钮 →
 * 断言交给 `host_config_set` 的整份 payload。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import overlaySrc from "../../../windows/OverlayShot.tsx?raw";
import { completeActions, completeLabel } from "../../../windows/overlay/postActions";
import ScreenshotPanel from "../ScreenshotPanel";
import {
  hostConfigGet,
  hostConfigSet,
  screenshotHistoryList,
  screenshotPins,
} from "../../../ipc/client";

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
// 面板订阅 nf:event 才接得上"配置被模块拒收"那句话（T-B4-9）；jsdom 里真 listen 会炸
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
    screenshotUploadTargets: vi.fn(async () => []),
    screenshotUpload: vi.fn(),
  };
});

/** 设置中心里已存在的截图配置（七键）：写回必须一枚不少地带回去 */
const EXISTING_CFG: Record<string, unknown> = {
  save_dir: "C:\\Users\\me\\shots",
  filename_template: "shot_{ts}.{fmt}",
  format: "jpeg",
  quality: 75,
  auto_copy: true,
  auto_save: true,
  auto_pin: false,
  post_actions: ["save"],
};

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(hostConfigGet).mockResolvedValue({ ...EXISTING_CFG });
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
  vi.mocked(screenshotPins).mockResolvedValue([]);
  vi.mocked(screenshotHistoryList).mockResolvedValue({ items: [], total: 0, page: 1, size: 30 });
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
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<ScreenshotPanel />);
  });
  await act(async () => {});
}

function chipByText(label: string): HTMLButtonElement {
  const found = [...container.querySelectorAll("button")].find((b) =>
    b.textContent?.includes(label),
  );
  if (!found) throw new Error(`找不到预设钮「${label}」`);
  return found as HTMLButtonElement;
}

/** 当前档 = aria-pressed（四枚同一时刻最多一枚按下；比类名哈希稳） */
function pressedLabels(): string[] {
  return [...container.querySelectorAll("button")]
    .filter((b) => b.getAttribute("aria-pressed") === "true")
    .map((b) => b.textContent ?? "");
}

describe("截图面板 · 完成后动作链预设（T-B4-8）", () => {
  it("shotSettings_actionPresets_writePostActionsViaHostConfigSet_mergedKeys", async () => {
    await mount();
    await act(async () => {
      chipByText("复制+OCR").click();
    });
    expect(hostConfigSet).toHaveBeenCalledTimes(1);
    const [module, payload] = vi.mocked(hostConfigSet).mock.calls[0] ?? [];
    expect(module).toBe("screenshot");
    // 红线：整份替换语义下漏键即写缺省值——既有七键一键不丢
    for (const [k, v] of Object.entries(EXISTING_CFG)) {
      if (k === "post_actions") continue;
      expect(payload).toHaveProperty(k, v);
    }
    expect((payload as Record<string, unknown>).post_actions).toEqual(["copy", "ocr"]);
    // 当前档翻成按下态（用户看得见自己是哪枚），且同一时刻只有那一枚
    expect(pressedLabels()).toEqual(["复制+OCR"]);
  });

  it("shotSettings_configAbsent_presetsDisabledAndWriteNeverFires", async () => {
    // 读设置失败（首帧还没拿到配置）：点预设必须零写入——`{...null}` 会把用户全部
    // 截图设置覆成只剩 post_actions 的一行
    vi.mocked(hostConfigGet).mockRejectedValue(new Error("boom"));
    await mount();
    expect(pressedLabels()).toEqual([]);
    await act(async () => {
      chipByText("仅保存").click();
    });
    expect(hostConfigSet).not.toHaveBeenCalled();
    const triggers = [...container.querySelectorAll("button")].filter((b) =>
      ["保存+复制", "仅保存", "复制+OCR", "贴图+复制"].some((l) => b.textContent?.includes(l)),
    );
    expect(triggers).toHaveLength(4);
    expect(triggers.every((b) => b.disabled)).toBe(true);
  });

  it("shotSettings_emptyPostActions_fallsBackToThreeBools_hintShown", async () => {
    // 旧快照（没有新键）：四枚都不是当前档，面板如实说"沿用三开关"而不是假装选中一枚
    vi.mocked(hostConfigGet).mockResolvedValue({ ...EXISTING_CFG, post_actions: [] });
    await mount();
    expect(document.body.textContent).toContain("沿用");
    expect(pressedLabels()).toEqual([]);
    // 正对照：三开关那一份配置不是"永远无当前档"——同份 cfg 换上非空名单即有一枚按下
    vi.mocked(hostConfigGet).mockResolvedValue({ ...EXISTING_CFG, post_actions: ["save"] });
    await act(async () => {
      root.unmount();
    });
    await mount();
    expect(pressedLabels()).toEqual(["仅保存"]);
  });
});

describe("覆盖层「完成」钮读 default_actions（T-B4-8）", () => {
  it("overlayFinish_completeButtonSendsDefaultActions", () => {
    // 模型半：钮传出去的正是宿主带下来的那份名单（顺序也原样，后端按序执行）
    expect(completeActions(["save", "copy"])).toEqual(["save", "copy"]);
    // 装配半：完成钮把 defaultActions 交给 finish，文案读同一个 state
    expect(overlaySrc).toContain("setDefaultActions(completeActions(info.default_actions))");
    expect(overlaySrc).toContain("onClick={() => void finish(defaultActions)}");
    expect(overlaySrc).toContain("completeLabel(defaultActions)");
    // 负例：升级前那颗写死空数组的钮不在了（写死 = 配置永远够不着覆盖层）
    expect(overlaySrc).not.toContain("finish([])");
  });

  it("overlayFinish_noDefaultActions_fallsBackToEmptyArray", () => {
    // 旧 TaskStartDto 无该键（undefined）与预热窗口重置后的 null：一律退回空数组，
    // 空数组在后端的语义就是"我没指定，按配置派生"，与升级前逐字一致
    expect(completeActions(undefined)).toEqual([]);
    expect(completeActions(null)).toEqual([]);
    expect(completeActions([])).toEqual([]);
    // URL 参数直进的覆盖层拿不到 TaskStartDto：初值就是空数组
    expect(overlaySrc).toContain("useState<string[]>([])");
    // 空名单不写成"完成（）"——空括号是 UI 上的谎
    expect(completeLabel(undefined)).toBe("完成");
    expect(completeLabel([])).toBe("完成");
    expect(completeLabel(["save", "copy"])).toBe("完成（保存+复制）");
    expect(completeLabel(["ocr"])).toBe("完成（识别）");
  });
});
