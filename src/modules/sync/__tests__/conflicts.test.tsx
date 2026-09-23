/**
 * 冲突历史视图（09 §10.2 T-B5-2）：败方快照要看得见，恢复要问一声。
 *
 * 三判据：
 * ① 行内展示的是**败方**内容快照（承重⑤的界面侧：查不回来就等于没落盘）；
 * ② 恢复是不可逆写操作 ⇒ 确认框没放行之前一个 invoke 都不许发（取消臂连刷新都不做）；
 * ③ 数据源是 `sync_conflicts_get`（落盘表），不是事件缓存——所以视图里不许出现"待实现"占位。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ConflictsSection, { snapshotPreview } from "../ConflictsSection";
import { confirmAction, impactLines } from "../../../stores/confirm";
import {
  syncConflictsGet,
  syncConflictRestore,
  type SyncConflictDto,
} from "../../../ipc/client";

vi.mock("../../../stores/confirm", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../stores/confirm")>();
  return { ...actual, confirmAction: vi.fn(async () => true) };
});

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    syncConflictsGet: vi.fn(),
    syncConflictRestore: vi.fn(),
  };
});

function row(over: Partial<SyncConflictDto> = {}): SyncConflictDto {
  return {
    conflictId: "c1".repeat(16),
    entity: "note",
    entityId: "想法.md",
    lostTs: Date.parse("2026-09-18T09:00:00"),
    lostDevice: "devB-ffffffff-0000",
    winnerDevice: "devA-11111111-0000",
    winnerTs: Date.parse("2026-09-18T09:30:00"),
    lostValue: { content: "这一版只在远端改过\n第二段", title: "想法" },
    recordedMs: Date.parse("2026-09-18T10:00:00"),
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(confirmAction).mockImplementation(async () => true);
  vi.mocked(syncConflictsGet).mockResolvedValue([row()]);
  vi.mocked(syncConflictRestore).mockResolvedValue({
    conflictId: "c1".repeat(16),
    entity: "note",
    entityId: "想法.md",
    opId: "op-new",
    ts: Date.parse("2026-09-23T10:00:00"),
  });
});

afterEach(() => {
  act(() => {
    root?.unmount();
  });
  container.remove();
  while (document.body.firstChild) document.body.removeChild(document.body.firstChild);
  vi.clearAllMocks();
});

async function mount() {
  await act(async () => {
    root = createRoot(container);
    root.render(<ConflictsSection />);
  });
  await act(async () => {});
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

/** 行内那枚恢复钮（每行一枚，按文案取，不碰 Fluent 哈希类名） */
function restoreButton(): HTMLButtonElement {
  const el = [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === "以本地副本重新生效并推送",
  ) as HTMLButtonElement | undefined;
  if (!el) throw new Error("行内没有恢复钮");
  return el;
}

describe("同步 · 冲突历史（T-B5-2）", () => {
  it("syncConflictRow_showsLoserSnapshot", async () => {
    await mount();
    expect(syncConflictsGet).toHaveBeenCalledTimes(1);
    // 败方原文（承重⑤的界面侧证据）+ 谁输给谁
    expect(bodyText()).toContain("这一版只在远端改过");
    expect(bodyText()).toContain("想法.md");
    expect(bodyText()).toContain("未采纳：devB-fff");
    expect(bodyText()).toContain("本机采纳：devA-111");
    // 真实数据源 ⇒ 不许有占位文案
    expect(bodyText()).not.toContain("待实现");

    // 快照形状两臂：删除快照不显示空内容，长正文按首行截断
    expect(snapshotPreview({ deleted: true })).toContain("一次删除");
    expect(snapshotPreview({ content: "x".repeat(300) })).toHaveLength(121);
  });

  it("syncConflict_restoreConfirmInvokesOnce", async () => {
    // ① 必须确认：确认框攥在测试手里时，invoke 一次都不该发出去
    let settle: (ok: boolean) => void = () => {};
    vi.mocked(confirmAction).mockImplementation(
      () =>
        new Promise<boolean>((resolve) => {
          settle = resolve;
        }),
    );
    await mount();
    await act(async () => {
      restoreButton().click();
    });

    expect(confirmAction).toHaveBeenCalledTimes(1);
    const opts = vi.mocked(confirmAction).mock.calls[0][0];
    expect(opts.danger).toBe(true);
    expect(opts.title).toBe("以本地副本重新生效并推送");
    // 文案红线：动作名与影响面不得承诺"撤销对端/强制回滚"（本机做不到）；
    // detail 里出现否定式说明（"不撤销对端"）恰恰是诚实，故断言只看动作名+影响面。
    const affordance = `${opts.title}${opts.confirmLabel ?? ""}${impactLines(opts).join("")}`;
    expect(affordance).not.toMatch(/撤销|回滚|强制/);
    expect(impactLines(opts).join("")).toContain("想法.md");
    expect(opts.detail).toContain("不撤销对端");
    expect(syncConflictRestore).not.toHaveBeenCalled();
    await act(async () => {
      settle?.(false);
    });
    expect(syncConflictRestore).not.toHaveBeenCalled();

    // ② 取消臂零 invoke：什么都没变 ⇒ 屏幕停在用户离开那一帧，连重取列表都不做
    await act(async () => {
      restoreButton().click();
    });
    expect(syncConflictRestore).not.toHaveBeenCalled();
    expect(vi.mocked(syncConflictsGet).mock.calls.length).toBe(1);

    // ③ 正对照：改判"确认"后同一路径真的入流并按表重取
    vi.mocked(confirmAction).mockImplementation(async () => true);
    await act(async () => {
      restoreButton().click();
    });
    await act(async () => {});
    expect(syncConflictRestore).toHaveBeenCalledTimes(1);
    expect(syncConflictRestore).toHaveBeenCalledWith(row().conflictId);
    expect(vi.mocked(syncConflictsGet).mock.calls.length).toBe(2);
    expect(bodyText()).toContain("已以本机副本重新生效");
  });

  it("syncConflictRow_restoreConfirmCancelZeroInvoke", async () => {
    // 取消臂单独钉一枚：确认框答"否"之后，既不发恢复命令，也不该顺手重取列表——
    // 什么都没变，屏幕就停在用户离开的那一帧（多余的刷新会把用户正在看的行序打乱）。
    vi.mocked(confirmAction).mockImplementation(async () => false);
    await mount();
    const btn = [...container.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "以本地副本重新生效并推送",
    ) as HTMLButtonElement;
    expect(btn, "行内没有恢复钮").toBeTruthy();

    await act(async () => {
      btn.click();
    });
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(syncConflictRestore).not.toHaveBeenCalled();
    expect(vi.mocked(syncConflictsGet).mock.calls.length).toBe(1);
    expect(bodyText()).not.toContain("已以本机副本重新生效");
  });

  it("syncConflictSnapshotViewer_isReadOnly", async () => {
    // §5-4 红线：快照查看是**只读**的——渲染树里不存在复制/导出/下载任何一键。
    // 败方内容是别台写过又被 LWW 判负的字句，做成一键外流就是给"已丢弃的记录"
    // 新开一个泄漏面；要拿回去只有「以本地副本重新生效」那条有账可查的路。
    await mount();
    const viewerBtn = [...container.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "查看快照",
    ) as HTMLButtonElement;
    expect(viewerBtn, "行内没有查看快照入口").toBeTruthy();
    await act(async () => {
      viewerBtn.click();
    });
    await act(async () => {});

    const surface = document.querySelector<HTMLElement>('[role="dialog"], .fui-DialogSurface');
    if (!surface) throw new Error("快照 Dialog 未打开");
    // 看得到全文（行内预览只给首行，Dialog 给整份）
    expect(surface.textContent ?? "").toContain("这一版只在远端改过");
    expect(surface.textContent ?? "").toContain("第二段");
    // 只读机检面：Dialog 内的按钮只有「关闭」，且没有输入面
    const labels = [...surface.querySelectorAll("button")].map(
      (b) => b.textContent?.trim() ?? "",
    );
    expect(labels).toEqual(["关闭"]);
    expect(surface.querySelector("input, textarea")).toBeNull();
    expect(surface.textContent ?? "").not.toMatch(/复制|导出|下载/);
  });
});
