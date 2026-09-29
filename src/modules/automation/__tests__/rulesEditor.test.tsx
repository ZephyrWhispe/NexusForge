import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import RulesPanel from "../RulesPanel";
import {
  automationDeadLetters,
  automationPluginsList,
  automationRulesList,
  automationRunsGet,
  automationSaveRule,
  type ExprDto,
  type RuleDto,
} from "../../../ipc/client";
import { confirmAction } from "../../../stores/confirm";

// D-29 B7/T-B7-13 回归：then 数组编辑器（五类动作全含 ipc_command + 上移/下移/
// 增删 + 整数组回填重存）、深层 when 未触碰逐字携带 / 触碰展平显式确认、
// 列表徽标「首动作 +N」。修前：单动作表单只回填 then[0]，三动作规则重存即丢两枚。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    automationRulesList: vi.fn(),
    automationDeadLetters: vi.fn(),
    automationRunsGet: vi.fn(),
    automationPluginsList: vi.fn(),
    automationSaveRule: vi.fn(),
  };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(),
}));

let container: HTMLDivElement;
let root: Root;

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll("button")].find(
    (b) => b.textContent?.trim() === text,
  );
}

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

function setInputValue(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

function inputByPlaceholder(prefix: string): HTMLInputElement | undefined {
  return [...document.querySelectorAll("input")].find((i) =>
    i.placeholder.startsWith(prefix),
  );
}

function baseRule(over: Partial<RuleDto>): RuleDto {
  return {
    id: "r1",
    name: "规则一",
    on: { kind: "event", topic: "clipboard.captured" },
    when: null,
    then: [{ kind: "notify", title: "A", body: "" }],
    cooldown_secs: 5,
    enabled: true,
    ...over,
  };
}

function mountWith(rule: RuleDto) {
  vi.mocked(automationRulesList).mockResolvedValue([rule]);
  return act(async () => {
    root = createRoot(container);
    root.render(<RulesPanel />);
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(automationDeadLetters).mockResolvedValue([]);
  vi.mocked(automationRunsGet).mockResolvedValue([]);
  vi.mocked(automationPluginsList).mockResolvedValue([]);
  vi.mocked(automationSaveRule).mockResolvedValue(undefined);
  vi.mocked(confirmAction).mockResolvedValue(true);
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

describe("RulesPanel 多动作编辑器（T-B7-13）", () => {
  it("rulesEditor_threeActionRule_resavesAllThree：三动作规则打开即回填整数组，原样重存 then 仍三枚", async () => {
    const rule = baseRule({
      then: [
        { kind: "notify", title: "A", body: "a" },
        { kind: "open_url", url: "https://x.test" },
        { kind: "publish", topic: "notes.changed", payload: { k: 1 } },
      ],
    });
    await mountWith(rule);
    await click(buttonByText("编辑")!);
    await click(buttonByText("保存")!);
    expect(automationSaveRule).toHaveBeenCalledTimes(1);
    const saved = vi.mocked(automationSaveRule).mock.calls[0][0];
    expect(saved.then).toHaveLength(3);
    expect(JSON.parse(JSON.stringify(saved.then))).toEqual(JSON.parse(JSON.stringify(rule.then)));
  });

  it("rulesEditor_moveAction_reordersPayload：↓ 移序后保存，payload 按新序外发", async () => {
    await mountWith(
      baseRule({
        then: [
          { kind: "notify", title: "第一", body: "" },
          { kind: "notify", title: "第二", body: "" },
          { kind: "notify", title: "第三", body: "" },
        ],
      }),
    );
    await click(buttonByText("编辑")!);
    const down = [...document.querySelectorAll("button")].filter((b) => b.textContent === "↓");
    expect(down.length, "三行应各有下移钮").toBe(3);
    await click(down[0]!);
    await click(buttonByText("保存")!);
    const titles = vi
      .mocked(automationSaveRule)
      .mock.calls[0][0].then.map((a) => (a.kind === "notify" ? a.title : a.kind));
    expect(titles).toEqual(["第二", "第一", "第三"]);
  });

  it("rulesEditor_ipcCommand_roundTripsParams：ipc_command 参数回填往返 + 非法 JSON 就地红不提交；现值即它时五枚全（D-41 条件渲染）", async () => {
    await mountWith(
      baseRule({
        then: [
          { kind: "ipc_command", module: "clipboard", cmd: "clipboard_get_entry", args: { x: 1 } },
        ],
      }),
    );
    await click(buttonByText("编辑")!);
    // 类型下拉：被编辑规则现值＝ipc_command ⇒ 五枚全（旧规则可读可存、数据不腐）
    const kindSelect = [...document.querySelectorAll("select")].find((s) =>
      [...s.options].some((o) => o.value === "ipc_command"),
    );
    expect(kindSelect, "现值为 ipc_command 时下拉须保留该项").toBeDefined();
    expect([...kindSelect!.options].map((o) => o.value)).toEqual([
      "notify",
      "open_url",
      "publish",
      "ipc_command",
      "run_script",
    ]);
    // 诚实化配套：这一臂后端恒 Err，故以延后徽标显影而非静默可选
    expect(document.body.textContent).toContain("IPC 命令动作 · 延后");
    // 回填：module/cmd/args 逐字带回
    expect(inputByPlaceholder("模块名")?.value).toBe("clipboard");
    expect(inputByPlaceholder("命令名")?.value).toBe("clipboard_get_entry");
    expect(inputByPlaceholder('{"id"')?.value).toBe('{"x":1}');
    // 非法 JSON：就地红 + 保存直接不提交（不发 IPC 调用）
    setInputValue(inputByPlaceholder('{"id"')!, "{bad");
    await act(async () => {});
    expect(document.body.textContent).toContain("IPC 参数不是合法 JSON，修正后才能保存");
    await click(buttonByText("保存")!);
    expect(automationSaveRule).not.toHaveBeenCalled();
    // 修好后按新参往返外发
    setInputValue(inputByPlaceholder('{"id"')!, '{"y":2}');
    await click(buttonByText("保存")!);
    expect(automationSaveRule).toHaveBeenCalledTimes(1);
    const action = vi.mocked(automationSaveRule).mock.calls[0][0].then[0];
    expect(action.kind).toBe("ipc_command");
    if (action.kind === "ipc_command") expect(action.args).toEqual({ y: 2 });
  });

  it("rulesEditor_ipcCommand_notOfferedForNewRows：新规则不给必然失败的入口（D-41 诚实化·上例的正面对立面）", async () => {
    await mountWith(baseRule({ then: [{ kind: "notify", title: "A", body: "" }] }));
    await click(buttonByText("新建规则")!);
    const kindSelect = [...document.querySelectorAll("select")].find((s) =>
      [...s.options].some((o) => o.value === "run_script"),
    );
    expect(kindSelect, "动作类型下拉未渲染").toBeDefined();
    expect([...kindSelect!.options].map((o) => o.value)).toEqual([
      "notify",
      "open_url",
      "publish",
      "run_script",
    ]);
    expect(document.body.textContent).not.toContain("IPC 命令动作 · 延后");
  });

  it("rulesEditor_deepWhen_untouchedSurvivesVerbatim：深层 when 未触碰 → 保存逐字原样携带（正对照防编辑器必然展平）", async () => {
    const deep: ExprDto = {
      op: "and",
      args: [
        { op: "leaf", args: { path: "entry.kind", cmp: "eq", value: "text" } },
        {
          op: "or",
          args: [
            { op: "leaf", args: { path: "entry.size", cmp: "gt", value: 10 } },
            { op: "not", args: { op: "leaf", args: { path: "entry.secret", cmp: "eq", value: true } } },
          ],
        },
      ],
    };
    await mountWith(baseRule({ when: deep }));
    await click(buttonByText("编辑")!);
    expect(document.body.textContent).toContain("深层嵌套条件树");
    await click(buttonByText("保存")!);
    const saved = vi.mocked(automationSaveRule).mock.calls[0][0];
    expect(JSON.stringify(saved.when)).toBe(JSON.stringify(deep));
  });

  it("rulesEditor_deepWhen_touchedAsksFlattenConfirm：触碰编辑器保存前点名「嵌套条件已展平为单层」确认，取消即中止", async () => {
    const deep: ExprDto = {
      op: "and",
      args: [
        { op: "leaf", args: { path: "a", cmp: "eq", value: 1 } },
        { op: "or", args: [{ op: "leaf", args: { path: "b", cmp: "eq", value: 2 } }] },
      ],
    };
    await mountWith(baseRule({ when: deep }));
    await click(buttonByText("编辑")!);
    await click(buttonByText("编辑条件（将降级为单层）")!);
    // 取消展平 → 中止保存，深层树不丢
    vi.mocked(confirmAction).mockResolvedValueOnce(false);
    await click(buttonByText("保存")!);
    expect(confirmAction).toHaveBeenCalledTimes(1);
    expect(vi.mocked(confirmAction).mock.calls[0][0].title).toContain("嵌套条件展平为单层");
    expect(automationSaveRule).not.toHaveBeenCalled();
    // 确认展平 → 保存的是编辑器单层形状（空叶被滤，when 落 null）
    await click(buttonByText("保存")!);
    expect(automationSaveRule).toHaveBeenCalledTimes(1);
    expect(vi.mocked(automationSaveRule).mock.calls[0][0].when).toBeNull();
  });

  it("rulesEditor_listBadge_showsFirstPlusCount：三动作列表徽标「首动作 +2」，单动作无 +N", async () => {
    await mountWith(
      baseRule({
        then: [
          { kind: "notify", title: "首条", body: "" },
          { kind: "publish", topic: "t1", payload: {} },
          { kind: "run_script", path: "plugin:demo", func: "run" },
        ],
      }),
    );
    expect(document.body.textContent).toContain("通知 · 首条 +2");
    vi.mocked(automationRulesList).mockResolvedValue([
      baseRule({ then: [{ kind: "notify", title: "只有一条", body: "" }] }),
    ]);
    await click(buttonByText("刷新")!);
    expect(document.body.textContent).toContain("通知 · 只有一条");
    expect(document.body.textContent).not.toContain("+1");
  });
});
