import { describe, expect, it } from "vitest";
import { isEditorDirty, serializeEditorForm } from "../VaultPanel";
import type { EntryFieldDto, VaultEntryDto } from "../../../ipc/client";

// D-18：编辑对话框关闭前的脏检测（纯函数，不渲染组件）
const baseFields: EntryFieldDto[] = [{ key: "password", kind: "password", value: "" }];

function makeEditor(over: Partial<Parameters<typeof serializeEditorForm>[0]> = {}) {
  const core = {
    entry: null,
    title: "",
    favorite: false,
    totpSecret: "",
    fields: baseFields,
    ...over,
  };
  return { ...core, genLength: 16, genUpper: true, genLower: true, genDigits: true, genSymbols: true, genAmbiguous: false, initial: serializeEditorForm(core) };
}

describe("isEditorDirty", () => {
  it("新建空表单未打开即未改动 → 不脏", () => {
    expect(isEditorDirty(makeEditor())).toBe(false);
  });

  it("填标题即脏", () => {
    const e = makeEditor();
    expect(isEditorDirty({ ...e, title: "GitHub" })).toBe(true);
  });

  it("改回原值不脏（编辑已有条目来回改动）", () => {
    const entry = {
      id: "e1",
      folder_id: null,
      title: "旧名",
      favorite: false,
      fields: baseFields,
      totp_secret: null,
      created_at: 0,
      updated_at: 0,
    } satisfies VaultEntryDto;
    const core = { entry, title: "旧名", favorite: false, totpSecret: "", fields: baseFields };
    const e = { ...makeEditor(core), initial: serializeEditorForm(core) };
    const edited = { ...e, title: "新名" };
    expect(isEditorDirty(edited)).toBe(true);
    expect(isEditorDirty({ ...edited, title: "旧名" })).toBe(false);
  });

  it("生成器参数变化不算脏（只是填值工具）", () => {
    const e = makeEditor();
    expect(isEditorDirty({ ...e, genLength: 32, genSymbols: false })).toBe(false);
  });

  it("字段增删算脏", () => {
    const e = makeEditor();
    expect(isEditorDirty({ ...e, fields: [] })).toBe(true);
  });
});
