import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import DesktopPanel from "../DesktopPanel";
import {
  desktopLauncherStatus,
  desktopNoteList,
  desktopTidyPlan,
  desktopTidyStatus,
  hostConfigGet,
  hostConfigSet,
  type DesktopNoteDto,
} from "../../../ipc/client";

// D-29 B7/T-B7-17（09 §7.2）：#标签点选过滤是"透传+精确匹配在 store 层"的分工——
// 前端只负责把 tag 参带进 desktop_note_list（Rust 侧 Vec<String> 全等匹配另有三枚测），
// 故本测断言的是**调用面**：设过滤/再点撤销两臂 + 过滤行显隐。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    desktopLauncherReindex: vi.fn(),
    desktopLauncherStatus: vi.fn(),
    desktopNoteAdd: vi.fn(),
    desktopNoteDone: vi.fn(),
    desktopNoteList: vi.fn(),
    desktopNoteRemove: vi.fn(),
    desktopTidyApply: vi.fn(),
    desktopTidyPlan: vi.fn(),
    desktopTidyRestore: vi.fn(),
    desktopTidyStatus: vi.fn(),
    hostConfigGet: vi.fn(),
    hostConfigSet: vi.fn(),
  };
});

vi.mock("../../../ipc/env", () => ({ IN_TAURI: true }));
vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

const note = (id: string, tags: string[], content: string): DesktopNoteDto => ({
  id,
  content,
  tags,
  remind_at: null,
  reminded: false,
  done: false,
  created_ms: 1_789_632_000_000,
});

const NOTES = [note("n1", ["work"], "周会材料 #work"), note("n2", ["workshop"], "兴趣班 #workshop")];

let container: HTMLDivElement;
let root: Root | null = null;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(desktopTidyStatus).mockResolvedValue(false);
  vi.mocked(desktopTidyPlan).mockResolvedValue({ groups: [], total: 0 });
  vi.mocked(desktopLauncherStatus).mockResolvedValue([true, 4]);
  vi.mocked(desktopNoteList).mockResolvedValue(NOTES);
  vi.mocked(hostConfigGet).mockResolvedValue({ tidy_map: null });
  vi.mocked(hostConfigSet).mockResolvedValue(undefined);
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  root = null;
  container.remove();
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<DesktopPanel />);
  });
  await act(async () => {});
}

async function clickTag(label: string) {
  const el = [...container.querySelectorAll("button")].find((b) => b.textContent === label);
  if (!el) throw new Error(`标签按钮 ${label} 未渲染`);
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

async function clickByText(text: string) {
  const el = [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
  if (!el) throw new Error(`按钮「${text}」未渲染`);
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

describe("随记标签过滤（T-B7-17）", () => {
  it("desktopPanel_tagClick_setsFilter_andClickAgainClears", async () => {
    await mount();
    // 初始：无 tag 参（旧调用零扰动的正对照臂）
    expect(vi.mocked(desktopNoteList).mock.lastCall).toEqual([false, undefined]);

    await clickTag("#work");
    expect(vi.mocked(desktopNoteList).mock.lastCall, "点标签必须透传 tag 参").toEqual([
      false,
      "work",
    ]);
    expect(container.textContent).toContain("过滤：#work");

    await clickTag("#work");
    expect(vi.mocked(desktopNoteList).mock.lastCall, "再点同标签=撤销过滤").toEqual([
      false,
      undefined,
    ]);
    expect(container.textContent).not.toContain("过滤：#work");

    // ✕ 按钮与点标签同语义：设上后可从过滤行撤销
    await clickTag("#workshop");
    expect(vi.mocked(desktopNoteList).mock.lastCall).toEqual([false, "workshop"]);
    await clickByText("✕");
    expect(vi.mocked(desktopNoteList).mock.lastCall).toEqual([false, undefined]);
  });

  it("desktopPanel_remindTriState_showsFiredDoneOrPending", async () => {
    // D-42：reminded 随列表下发却从未上屏——"到点了到底敲过我一次没有"只能猜。
    // 三态各一臂并按行配对断言（只数命中数会漏掉"两行状态对调"这类错）。
    vi.mocked(desktopNoteList).mockResolvedValue([
      { ...note("r1", [], "交周报"), remind_at: 1_789_700_000_000, reminded: true },
      { ...note("r2", [], "买牛奶"), remind_at: 1_789_700_000_000, done: true },
      { ...note("r3", [], "打疫苗"), remind_at: 1_789_700_000_000 },
      // 负对照：没有 remind_at 的行不该冒出任何提醒态后缀
      note("r4", [], "无提醒"),
    ]);
    await mount();

    // 提醒态尾巴挂在 remind span 上，其所在行的正文 = 该 span 祖先块的第一个 div
    const stateOf = (content: string) =>
      [...container.querySelectorAll("span")]
        .filter((s) =>
          /(?:已提醒|已完成不再提醒|待提醒)$/.test((s.textContent ?? "").trim()) &&
          (s.parentElement?.parentElement?.firstElementChild?.textContent ?? "").startsWith(
            content,
          ),
        )
        .map((s) => (s.textContent ?? "").trim().replace(/^.*·\s*/, ""))
        .join("|");

    expect(stateOf("交周报")).toBe("已提醒");
    expect(stateOf("买牛奶")).toBe("已完成不再提醒");
    expect(stateOf("打疫苗")).toBe("待提醒");
    expect(stateOf("无提醒")).toBe("");
  });
});
