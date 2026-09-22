/**
 * 历史行「删除」（09 §9.2 T-B4-14）：不可逆操作三道判据。
 *
 * ① **必须确认**：确认框没点头之前，一个 invoke 都不许发出去（不是"点完再问"）；
 * ② **取消零 invoke**：取消那一臂连"重取列表"都不该发生——什么都没变，刷新是假动作；
 * ③ **删后重取**：行没了这件事由宿主的列表说了算，面板不本地滤掉那一行。
 *    第 ③ 条的正反两对照都在测里：宿主回"[s1, s2]"（它还没落下手）时面板就显示两行，
 *    宿主回"[s1]"时才显示一行——只信返回的那份，才可能在文件其实没删掉时如实显示。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ScreenshotPanel from "../ScreenshotPanel";
import { confirmAction } from "../../../stores/confirm";
import {
  hostConfigGet,
  screenshotHistoryDelete,
  screenshotHistoryGet,
  screenshotHistoryList,
  screenshotPinGet,
  screenshotPins,
  screenshotUploadTargets,
  type ShotItemDto,
} from "../../../ipc/client";

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    hostConfigGet: vi.fn(),
    screenshotHistoryList: vi.fn(),
    screenshotHistoryGet: vi.fn(),
    screenshotHistoryCopy: vi.fn(),
    screenshotHistoryDelete: vi.fn(),
    screenshotPins: vi.fn(),
    screenshotPinGet: vi.fn(),
    screenshotUploadTargets: vi.fn(),
  };
});

/** 两行尺寸各异：断言看的是各自那行文案，不需要碰 Fluent 的哈希类名 */
function shot(id: string, file: string | null): ShotItemDto {
  return {
    id,
    created_ms: Date.parse("2026-09-19T10:30:00"),
    width: id === "s1" ? 100 : 200,
    height: id === "s1" ? 50 : 60,
    file,
    ocr_text: null,
  };
}

function page(items: ShotItemDto[]) {
  return { items, total: items.length, page: 1, size: 30 };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(confirmAction).mockImplementation(async () => true);
  vi.mocked(hostConfigGet).mockResolvedValue({});
  vi.mocked(screenshotUploadTargets).mockResolvedValue([]);
  vi.mocked(screenshotHistoryList).mockResolvedValue(
    page([shot("s1", "C:\\shots\\s1.png"), shot("s2", "C:\\shots\\s2.png")]),
  );
  vi.mocked(screenshotHistoryGet).mockImplementation(async (id) => ({
    id,
    png_b64: "QUJD",
    format: "image/png",
    annotations: [],
  }));
  vi.mocked(screenshotHistoryDelete).mockResolvedValue(undefined);
  vi.mocked(screenshotPins).mockResolvedValue([]);
  vi.mocked(screenshotPinGet).mockResolvedValue({
    id: "p",
    x: 0,
    y: 0,
    width: 1,
    height: 1,
    zoom: 1,
    opacity: 1,
    png_b64: "AA",
  });
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
  // Fluent 菜单在 body 上留门户节点，逐个摘掉而不是重写 innerHTML：
  // 后者会被 B3 的注入面红线扫描器（htmlFormat.test.tsx）当成新增宿主判红。
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

function bodyText(): string {
  return document.body.textContent ?? "";
}

/** 某一行的溢出入口（每行一枚「更多」，里面只有「删除」） */
function moreTrigger(row: number): HTMLButtonElement {
  const triggers = [...container.querySelectorAll("button")].filter(
    (b) => b.textContent?.trim() === "更多",
  ) as HTMLButtonElement[];
  const el = triggers[row];
  if (!el) throw new Error(`第 ${row} 行没有「更多」溢出入口（共 ${triggers.length} 枚）`);
  return el;
}

/** 点开指定行的溢出菜单并把「删除」那一项交给调用方 */
async function clickDelete(row: number) {
  await act(async () => {
    moreTrigger(row).click();
  });
  const item = [...document.body.querySelectorAll('[role="menuitem"]')].find(
    (el) => el.textContent?.trim() === "删除",
  ) as HTMLElement | undefined;
  if (!item) throw new Error("溢出菜单里没有「删除」项");
  await act(async () => {
    item.click();
  });
}

describe("截图面板 · 历史行删除（T-B4-14）", () => {
  it("shotPanel_delete_requiresConfirm_thenInvokes", async () => {
    // 确认框由测试攥着：它没放行之前，invoke 一次都不该发出去
    let settle: (ok: boolean) => void = () => {};
    vi.mocked(confirmAction).mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          settle = resolve;
        }),
    );
    await mount();
    await clickDelete(0);

    expect(confirmAction).toHaveBeenCalledTimes(1);
    const opts = vi.mocked(confirmAction).mock.calls[0][0];
    expect(opts.danger).toBe(true);
    expect(opts.impact).toBe("从历史与磁盘同时移除该截图文件（不进回收站）");
    // command 预览带的是被点那一行的 id，不是全局某条
    expect(opts.command).toBe("s1");
    expect(screenshotHistoryDelete).not.toHaveBeenCalled();

    // 正对照：放行之后同一趟点击就真的删了
    await act(async () => {
      settle?.(true);
    });
    expect(screenshotHistoryDelete).toHaveBeenCalledTimes(1);
    expect(screenshotHistoryDelete).toHaveBeenCalledWith("s1");
  });

  it("shotPanel_delete_cancelZeroInvoke", async () => {
    vi.mocked(confirmAction).mockImplementation(async () => false);
    await mount();
    const listsBefore = vi.mocked(screenshotHistoryList).mock.calls.length;
    await clickDelete(0);

    expect(confirmAction).toHaveBeenCalledTimes(1);
    // 红线取消臂：什么都没发生才算"取消"——连刷新都不做，屏幕就停在用户离开时那一帧
    expect(screenshotHistoryDelete).not.toHaveBeenCalled();
    expect(vi.mocked(screenshotHistoryList).mock.calls.length).toBe(listsBefore);
    expect(bodyText()).toContain("200×60");

    // 正对照：同一处代码改判"确认"，两次 invoke 就都到（0 不是永远 0 蒙出来的）
    vi.mocked(confirmAction).mockImplementation(async () => true);
    await clickDelete(0);
    expect(screenshotHistoryDelete).toHaveBeenCalledTimes(1);
  });

  it("shotPanel_delete_refreshesListAfterSuccess", async () => {
    await mount();
    // 宿主说"两条都还在"（它那一侧尚未落手）：面板跟着显示两条，不自作主张抹掉一行
    await clickDelete(1);
    expect(screenshotHistoryDelete).toHaveBeenCalledWith("s2");
    expect(vi.mocked(screenshotHistoryList).mock.calls.length).toBe(2);
    expect(bodyText()).toContain("200×60");

    // 正对照：宿主下一次真的只回一条，面板才少一行
    vi.mocked(screenshotHistoryList).mockResolvedValue(page([shot("s1", "C:\\shots\\s1.png")]));
    await clickDelete(0);
    expect(vi.mocked(screenshotHistoryList).mock.calls.length).toBe(3);
    expect(bodyText()).not.toContain("200×60");
    expect(bodyText()).toContain("100×50");
  });
});
