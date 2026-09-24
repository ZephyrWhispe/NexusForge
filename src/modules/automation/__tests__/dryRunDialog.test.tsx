import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import RulesPanel from "../RulesPanel";
import DryRunDialog from "../../../components/DryRunDialog";
import {
  automationDeadLetters,
  automationDryRun,
  automationPluginsList,
  automationReplay,
  automationRulesList,
  automationRunsGet,
  type DeadLetterDto,
} from "../../../ipc/client";

// D-29 B7/T-B7-15 回归：通用「干跑预览→清单确认→执行」闸（共性③首例）——
// onConfirm 仅在「确认执行」后被调（干跑阶段零端口触达的前端镜像）；
// 死信批量重放 = 循环既有 replay + 逐条结果汇总。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return {
    ...actual,
    automationRulesList: vi.fn(),
    automationDeadLetters: vi.fn(),
    automationRunsGet: vi.fn(),
    automationPluginsList: vi.fn(),
    automationDryRun: vi.fn(),
    automationReplay: vi.fn(),
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

function deadLetter(id: string, over: Partial<DeadLetterDto> = {}): DeadLetterDto {
  return {
    id,
    rule_id: "r1",
    rule_name: "规则一",
    action: { kind: "open_url", url: "https://fail" },
    error: `open 失败: ${id}`,
    at_ms: 1,
    ...over,
  };
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(automationRulesList).mockResolvedValue([]);
  vi.mocked(automationDeadLetters).mockResolvedValue([]);
  vi.mocked(automationRunsGet).mockResolvedValue([]);
  vi.mocked(automationPluginsList).mockResolvedValue([]);
  vi.mocked(automationDryRun).mockResolvedValue([]);
  vi.mocked(automationReplay).mockResolvedValue(undefined);
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

describe("DryRunDialog 通用确认闸（T-B7-15 共性③首例）", () => {
  it("dryRunDialog_confirmGate_executesOnlyAfterOk：onConfirm 与业务解耦——取消/Esc 不触发，仅「确认执行」后恰好一次", async () => {
    const onConfirm = vi.fn(async () => {});
    const onCancel = vi.fn();
    await act(async () => {
      root = createRoot(container);
      root.render(
        <DryRunDialog
          open
          title="干跑预览 · 示例"
          items={[
            { text: "notify 标题「A」 · 仅展示", risky: false },
            { text: "run_script plugin:demo::run · 真执行风险", risky: true },
          ]}
          onConfirm={onConfirm}
          onCancel={onCancel}
        />,
      );
    });
    // 两标展示正对照：清单渲染即含「仅展示/真执行风险」徽标
    expect(document.body.textContent).toContain("仅展示");
    expect(document.body.textContent).toContain("真执行风险");
    // 未确认前零执行（红线的前端镜像：干跑阶段什么都不发）
    expect(onConfirm).not.toHaveBeenCalled();
    await click(buttonByText("取消")!);
    expect(onConfirm).not.toHaveBeenCalled();
    expect(onCancel).toHaveBeenCalledTimes(1);
    // 确认执行 → 恰一次，且是唯一入口
    await click(buttonByText("确认执行")!);
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });
});

describe("RulesPanel 死信批量重放（T-B7-15）", () => {
  it("deadReplay_batch汇总CountsPerLetter：批量重放先过清单确认闸（未确认零 replay），确认后逐条循环既有命令并汇总成功/失败计数", async () => {
    const letters = [deadLetter("d1"), deadLetter("d2", { rule_id: "r2", rule_name: "规则二" })];
    vi.mocked(automationDeadLetters).mockResolvedValue(letters);
    await act(async () => {
      root = createRoot(container);
      root.render(<RulesPanel />);
    });
    await click(buttonByText("死信（2）")!);
    await click(buttonByText("批量重放")!);
    // 三步闸正对照：预览已开（逐条死信在清单上）但执行未启动
    expect(document.body.textContent).toContain("批量重放 · 2 条死信");
    expect(document.body.textContent).toContain("open 失败: d1");
    expect(automationReplay).not.toHaveBeenCalled();
    // 确认 → 循环既有 replay 逐条外发（id+rule_id 逐条对应）
    await click(buttonByText("确认重放全部")!);
    expect(vi.mocked(automationReplay).mock.calls).toEqual([
      ["d1", "r1"],
      ["d2", "r2"],
    ]);
    // 逐条结果汇总：d2 模拟失败臂 → 计数各归各
    vi.mocked(automationReplay).mockReset();
    vi.mocked(automationReplay).mockImplementation(async (id) => {
      if (id === "d2") throw new Error("仍失败");
    });
    await click(buttonByText("批量重放")!);
    await click(buttonByText("确认重放全部")!);
    expect(document.body.textContent).toContain("批量重放汇总：成功 1 · 失败 1");
  });
});
