import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";

import ConnectDialog from "../ConnectDialog";
import { fileRemoteConnect, type RemoteDriverDto, type RemoteProfileDto } from "../../../ipc/client";
import { notify, reportError } from "../../../stores/notifications";
import { confirmAction } from "../../../stores/confirm";

// D-29 B6/T-B6-8 回归：authKind 换档 = 整表单重挂载（口令不可能跨档残留）；
// 明文第三闸走 FILE_REMOTE_008 → confirmAction 复述地址 → 仅本次携参重试。

vi.mock("../../../ipc/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../ipc/client")>();
  return { ...actual, fileRemoteConnect: vi.fn() };
});

vi.mock("../../../stores/notifications", () => ({
  notify: vi.fn(),
  reportError: vi.fn(),
}));

vi.mock("../../../stores/confirm", () => ({
  confirmAction: vi.fn(async () => true),
}));

function ftpProfile(over: Partial<RemoteProfileDto> = {}): RemoteProfileDto {
  return {
    id: "remote:ftp-lab",
    label: "联调 FTP",
    protocol: "ftp",
    host: "127.0.0.2",
    port: 21,
    user: "me",
    base_path: "/",
    auth: { kind: "prompt_each_time" },
    preset_id: null,
    last_used_ms: 0,
    ...over,
  };
}

function driverInfo(): RemoteDriverDto {
  return {
    driver_id: "remote:ftp-lab",
    label: "联调 FTP",
    protocol: "ftp",
    host: "127.0.0.2",
    port: 21,
    base_path: "/",
    roots: ["/"],
    auth_source: "typed",
  };
}

let container: HTMLDivElement;
let root: Root;

async function click(el: Element) {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await act(async () => {});
}

function connectButton(): HTMLButtonElement {
  const b = [...document.querySelectorAll("button")].find(
    (x) => x.textContent?.trim() === "连接",
  );
  return b as HTMLButtonElement;
}

function passInput(): HTMLInputElement {
  return document.getElementById("cd-pass") as HTMLInputElement;
}

/** 受控输入必须走原生 setter + input 事件，直接赋 .value 进不了 React state */
async function typePass(value: string) {
  const input = passInput();
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** Fluent Radio 的 input[type=radio] 顺序 = 组件内档位声明序 */
function radioByIndex(i: number): HTMLInputElement {
  return document.querySelectorAll('input[type="radio"]')[i] as HTMLInputElement;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  vi.mocked(fileRemoteConnect).mockReset();
  vi.mocked(confirmAction).mockReset();
  vi.mocked(confirmAction).mockResolvedValue(true);
  vi.mocked(notify).mockReset();
  vi.mocked(reportError).mockReset();
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
});

function renderDialog(profile: RemoteProfileDto, onConnected?: (info: RemoteDriverDto) => void) {
  root = createRoot(container);
  act(() => {
    root.render(
      <ConnectDialog profile={profile} open onOpenChange={() => {}} onConnected={onConnected} />,
    );
  });
}

describe("ConnectDialog（T-B6-8 形状先行）", () => {
  it("connectionDialog_authKindSwitch_rerendersWholeForm", async () => {
    renderDialog(ftpProfile());
    const oldSection = document.querySelector('[data-testid="auth-form-prompt_each_time"]');
    expect(oldSection).toBeTruthy();
    await typePass("typed-PASS-should-die");

    // 换到 vault_entry 档（声明序第 3 枚）：整表单重挂载的 DOM 级证据 =
    // 旧 section 节点脱离文档 + 新档位输入框从零开始
    await click(radioByIndex(2));
    expect(oldSection?.isConnected).toBe(false);
    const newSection = document.querySelector('[data-testid="auth-form-vault_entry"]');
    expect(newSection?.isConnected).toBe(true);
    expect(document.getElementById("cd-pass")).toBeNull();
    const entryInput = document.getElementById("cd-entryid") as HTMLInputElement;
    expect(entryInput.value).toBe("");
    expect(newSection?.textContent).not.toContain("typed-PASS-should-die");
  });

  it("plaintextNonLoopback_008复述地址确认后仅本次携参重试", async () => {
    const err008 = {
      kind: "Network",
      data: { code: "FILE_REMOTE_008", message: "明文连接需用户明示（逐次）：已在建连之前拒绝" },
    };
    vi.mocked(fileRemoteConnect)
      .mockRejectedValueOnce(err008)
      .mockResolvedValueOnce(driverInfo());
    const onConnected = vi.fn();
    renderDialog(ftpProfile(), onConnected);

    await typePass("ftp-PASS");
    await click(connectButton());

    expect(fileRemoteConnect).toHaveBeenCalledTimes(2);
    expect(fileRemoteConnect).toHaveBeenNthCalledWith(
      1,
      "remote:ftp-lab",
      { password: "ftp-PASS" },
      false,
    );
    // 确认对话框复述目标地址（用户对着"是什么"说 yes，不是对着错误码）
    const args = vi.mocked(confirmAction).mock.calls[0][0];
    const impact = [args.impact].flat().join("\n");
    expect(impact).toContain("127.0.0.2:21");
    expect(fileRemoteConnect).toHaveBeenNthCalledWith(
      2,
      "remote:ftp-lab",
      { password: "ftp-PASS" },
      true,
    );
    expect(onConnected).toHaveBeenCalledWith(expect.objectContaining({ auth_source: "typed" }));
    // 提交后口令就地清空（组件 state 不养鱼）
    expect(passInput().value).toBe("");
  });

  it("plaintext_008用户拒绝则不重试且档案面零触碰", async () => {
    vi.mocked(fileRemoteConnect).mockRejectedValueOnce({
      kind: "Network",
      data: { code: "FILE_REMOTE_008", message: "明文连接需用户明示（逐次）" },
    });
    vi.mocked(confirmAction).mockResolvedValue(false);
    renderDialog(ftpProfile());
    await click(connectButton());
    expect(fileRemoteConnect).toHaveBeenCalledTimes(1);
    expect(vi.mocked(confirmAction)).toHaveBeenCalledTimes(1);
    const alert = document.querySelector('[role="alert"]');
    expect(alert?.textContent).toContain("FILE_REMOTE_008");
  });
});
