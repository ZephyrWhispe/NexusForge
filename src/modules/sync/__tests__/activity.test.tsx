/**
 * 同步活动流水视图（09 §10.2 T-B5-3）：每轮会话——尤其是失败的那轮——必须看得见。
 *
 * 判据面：
 * ① 失败行不被过滤掉（红线"失败不静默"：只保留成功行的列表等于把事故咽下去）；
 * ② 计数如实展示（推送/拉取应用/丢弃/冲突四项来自落盘行，不是事件缓存）；
 * ③ 角色区分本机发起/对端发起（承重④：被动侧结果同表可查），
 *    且握手前的行把 socket 地址如实贴出而非编造设备身份。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ActivitySection, { peerLabel } from "../ActivitySection";
import { syncRunsGet, type SyncRunDto } from "../../../ipc/client";

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return { ...actual, syncRunsGet: vi.fn() };
});

function run(over: Partial<SyncRunDto> = {}): SyncRunDto {
  return {
    id: 1,
    tsMs: Date.parse("2026-09-18T10:00:00"),
    peer: "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    role: "initiator",
    pushed: 2,
    pulledApplied: 3,
    pulledLost: 1,
    conflicts: 0,
    durationMs: 145,
    error: null,
    ...over,
  };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(syncRunsGet).mockResolvedValue([run()]);
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
    root.render(<ActivitySection />);
  });
  await act(async () => {});
}

function bodyText(): string {
  return document.body.textContent ?? "";
}

function refreshButton(): HTMLButtonElement {
  const el = [...container.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === "刷新",
  ) as HTMLButtonElement | undefined;
  if (!el) throw new Error("没有刷新钮");
  return el;
}

describe("同步 · 活动流水（T-B5-3）", () => {
  it("syncActivityRow_showsPushPullError", async () => {
    await mount();
    expect(syncRunsGet).toHaveBeenCalledTimes(1);
    // 成功行：四项计数 + 角色 + 耗时，全部来自落盘行
    expect(bodyText()).toContain("推送 2");
    expect(bodyText()).toContain("拉取应用 3");
    expect(bodyText()).toContain("丢弃 1");
    expect(bodyText()).toContain("冲突 0");
    expect(bodyText()).toContain("本机发起");
    expect(bodyText()).toContain("145 ms");
    expect(bodyText()).toContain("成功");
    expect(bodyText()).not.toContain("待实现");

    // 失败行必须在场（"失败不静默"）：错误文本原样展示，本轮已完成的部分计数照记，
    // 且同一读口换回一条失败行时上一行不会残留（列表是表读视图，不是事件累加）
    vi.mocked(syncRunsGet).mockResolvedValue([
      run({
        id: 2,
        role: "responder",
        pushed: 1,
        pulledApplied: 0,
        pulledLost: 0,
        conflicts: 1,
        error: "对端握手失败：指纹与配对记录不符",
      }),
    ]);
    await act(async () => {
      refreshButton().click();
    });
    await act(async () => {});
    expect(syncRunsGet).toHaveBeenCalledTimes(2);
    expect(bodyText()).toContain("对端握手失败：指纹与配对记录不符");
    expect(bodyText()).toContain("失败");
    expect(bodyText()).toContain("对端发起");
    expect(bodyText()).toContain("推送 1");
    expect(bodyText()).not.toContain("成功");

    // 空表 + 首轮未落定 ⇒ 加载态而非假空态（D-18 纪律沿用）
    act(() => {
      root.unmount();
    });
    vi.mocked(syncRunsGet).mockImplementation(() => new Promise(() => {}));
    await mount();
    expect(bodyText()).not.toContain("暂无同步流水");
  });

  it("syncActivity_errorRowRedBadge", async () => {
    // "标红"的机检面不是文本：失败徽标的类名必须与成功徽标不同色（同一条 CSS 通道 =
    // 根本没分色，只是文案换了字），且错误原文那一行与普通计数行分属两类——
    // 与 status.test 的 warnClassDiffers 同一手法：只比样式分野，不猜 Fluent 的哈希类名。
    vi.mocked(syncRunsGet).mockResolvedValue([run({ error: "连接被拒绝" })]);
    await mount();
    const badgeClass = (text: string) =>
      [...container.querySelectorAll(".fui-Badge")].find(
        (b) => b.textContent?.trim() === text,
      )?.className;
    const failClass = badgeClass("失败");
    expect(failClass, "失败行没有徽标").toBeTruthy();
    expect(bodyText()).toContain("连接被拒绝");

    // 错误原文与相邻计数行分属两类样式（红字 vs muted）
    const errLine = [...container.querySelectorAll("div")].find(
      (d) => d.textContent === "连接被拒绝" && d.children.length === 0,
    );
    const mutedLine = [...container.querySelectorAll("span")].find((s) =>
      s.textContent?.startsWith("推送 "),
    );
    expect(errLine, "错误原文未单独成行").toBeTruthy();
    expect(mutedLine, "计数行未渲染").toBeTruthy();
    expect(errLine!.className).not.toBe(mutedLine!.className);

    // 正对照：同一读口换回一条成功行 ⇒ 徽标换色。取的是**类名字符串**而非节点引用——
    // React 复用同一 DOM 节点，握着旧节点比等于自己跟自己比（第一轮就栽在这里）。
    vi.mocked(syncRunsGet).mockResolvedValue([run({ error: null })]);
    await act(async () => {
      refreshButton().click();
    });
    await act(async () => {});
    const okClass = badgeClass("成功");
    if (!okClass) throw new Error("成功行没有徽标");
    expect(okClass).not.toBe(failClass);
  });

  it("syncActivityRow_preHandshakePeerShowsAddress", () => {
    // 握手前的失败行只有 socket 地址：如实贴地址，不编"未知设备"
    expect(peerLabel("127.0.0.1:49899")).toBe("地址 127.0.0.1:49899（未完成握手）");
    // 握手后是设备 id：截前 8 位可读，不整串糊满一行
    expect(peerLabel("a1b2c3d4-e5f6-7890-abcd-ef1234567890")).toBe("a1b2c3d4");
  });
});
