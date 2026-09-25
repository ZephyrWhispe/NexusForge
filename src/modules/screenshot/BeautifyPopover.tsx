import { useCallback, useState } from "react";
import {
  Button,
  Input,
  makeStyles,
  Spinner,
  Switch,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  screenshotBeautifyApply,
  type BeautifySpecDto,
} from "../../ipc/client";
import { notify, reportError } from "../../stores/notifications";
import { BEAUTIFY_BG } from "../../theme/palette";

/**
 * 美化导出弹层（D-29 B4 T-B4-6）：内边距/圆角/投影 + 双色渐变底 + 预览 + 导出。
 *
 * 预览走 `screenshot_beautify_apply(id, spec, [])`——**空 actions 分支在宿主侧零落盘、
 * 零剪贴板写**，所以这里预览成功不弹任何"已保存"式提示（弹了就等于骗用户写了盘）。
 * 导出走同一命令、带面板菜单选定的那一个动作，不另开第二条导出通路。
 */

/** 表单态（数值以字符串存：输入框允许中间态，提交前一次性解析） */
export interface BeautifyFields {
  radius: string;
  padding: string;
  shadow: boolean;
  bgFrom: string;
  bgTo: string;
}

/** 表单态 → 线上 spec。非数字/负数收为 0，颜色原样交宿主校验（坏值点名报错，前端不猜） */
export function toBeautifySpec(f: BeautifyFields): BeautifySpecDto {
  const nonNeg = (s: string) => {
    const v = Math.floor(Number(s));
    return Number.isFinite(v) && v > 0 ? v : 0;
  };
  return {
    radius: nonNeg(f.radius),
    padding: nonNeg(f.padding),
    shadow: f.shadow,
    bg_from: f.bgFrom.trim(),
    bg_to: f.bgTo.trim(),
  };
}

/**
 * 三预设 chip（任务书面：无 / 圆角阴影 / 社交卡片）。
 * "无"即恒等预设（全 0 + 同色），点它等于把图原样导出一遍——它必须在，
 * 因为它是"美化没改图"的可用证据。
 */
export const BEAUTIFY_PRESETS: { label: string; fields: BeautifyFields }[] = [
  {
    label: "无",
    fields: { radius: "0", padding: "0", shadow: false, bgFrom: BEAUTIFY_BG.none.from, bgTo: BEAUTIFY_BG.none.to },
  },
  {
    label: "圆角阴影",
    fields: { radius: "24", padding: "32", shadow: true, bgFrom: BEAUTIFY_BG.card.from, bgTo: BEAUTIFY_BG.card.to },
  },
  {
    label: "社交卡片",
    fields: { radius: "40", padding: "96", shadow: true, bgFrom: BEAUTIFY_BG.social.from, bgTo: BEAUTIFY_BG.social.to },
  },
];

const useStyles = makeStyles({
  root: {
    display: "flex",
    flexDirection: "column",
    gap: "8px",
    padding: "10px 12px 12px",
    margin: "0 0 12px",
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground2,
  },
  heading: {
    fontSize: tokens.fontSizeBase300,
    fontWeight: tokens.fontWeightSemibold,
  },
  chips: { display: "flex", gap: "6px", flexWrap: "wrap" },
  row: { display: "flex", alignItems: "center", gap: "10px", flexWrap: "wrap" },
  field: { display: "flex", flexDirection: "column", gap: "2px" },
  label: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground3,
  },
  preview: {
    maxWidth: "360px",
    maxHeight: "220px",
    objectFit: "contain",
    border: `1px solid ${tokens.colorNeutralStroke2}`,
    borderRadius: tokens.borderRadiusMedium,
    backgroundColor: tokens.colorNeutralBackground1,
  },
  previewEmpty: {
    fontSize: tokens.fontSizeBase200,
    color: tokens.colorNeutralForeground4,
  },
  ops: { display: "flex", gap: "6px" },
});

export interface BeautifyTarget {
  id: string;
  /** 行内展示用（"美化另存" / "美化复制"） */
  label: string;
  /** 导出时带上的动作（宿主词表：save | copy | pin） */
  actions: string[];
}

export default function BeautifyPopover({
  target,
  onClose,
}: {
  target: BeautifyTarget;
  onClose: () => void;
}) {
  const styles = useStyles();
  const [fields, setFields] = useState<BeautifyFields>(BEAUTIFY_PRESETS[1].fields);
  const [preview, setPreview] = useState<string | null>(null);
  const [busy, setBusy] = useState<"preview" | "export" | null>(null);

  const set = useCallback(<K extends keyof BeautifyFields>(key: K, value: BeautifyFields[K]) => {
    setFields((prev) => ({ ...prev, [key]: value }));
  }, []);

  /** 预览一次（spec 显式传参，不吃 setState 的异步——chip 点下去要看到"那一张"） */
  const runPreview = useCallback(
    async (spec: BeautifySpecDto) => {
      setBusy("preview");
      try {
        const res = await screenshotBeautifyApply(target.id, spec, []);
        setPreview(res.preview_b64 ? `data:image/png;base64,${res.preview_b64}` : null);
      } catch (e) {
        reportError(e, { context: "美化预览失败", dedupeKey: `beautify-preview-${target.id}` });
      } finally {
        setBusy(null);
      }
    },
    [target.id],
  );

  const applyPreset = useCallback(
    (preset: (typeof BEAUTIFY_PRESETS)[number]) => {
      setFields(preset.fields);
      void runPreview(toBeautifySpec(preset.fields));
    },
    [runPreview],
  );

  const runExport = useCallback(async () => {
    setBusy("export");
    try {
      const res = await screenshotBeautifyApply(target.id, toBeautifySpec(fields), target.actions);
      const where = target.actions.includes("copy") ? "已复制到剪贴板" : "美化图已导出";
      notify("success", where, res.file ?? "派生文件，不新增历史记录");
      onClose();
    } catch (e) {
      reportError(e, { context: "美化导出失败", dedupeKey: `beautify-export-${target.id}` });
    } finally {
      setBusy(null);
    }
  }, [fields, target.actions, target.id, onClose]);

  return (
    <div className={styles.root}>
      <Text className={styles.heading}>美化导出 · {target.label}</Text>
      <div className={styles.chips}>
        {BEAUTIFY_PRESETS.map((p) => (
          <Button key={p.label} size="small" onClick={() => applyPreset(p)}>
            {p.label}
          </Button>
        ))}
      </div>
      <div className={styles.row}>
        <div className={styles.field}>
          <span className={styles.label}>圆角(px)</span>
          <Input
            size="small"
            aria-label="圆角半径"
            style={{ width: "88px" }}
            value={fields.radius}
            onChange={(_, d) => set("radius", d.value)}
          />
        </div>
        <div className={styles.field}>
          <span className={styles.label}>内边距(px)</span>
          <Input
            size="small"
            aria-label="内边距"
            style={{ width: "88px" }}
            value={fields.padding}
            onChange={(_, d) => set("padding", d.value)}
          />
        </div>
        <Switch
          label="投影"
          checked={fields.shadow}
          onChange={(_, d) => set("shadow", d.checked)}
        />
      </div>
      <div className={styles.row}>
        <div className={styles.field}>
          <span className={styles.label}>渐变起色 #RRGGBB</span>
          <Input
            size="small"
            aria-label="渐变起色"
            style={{ width: "120px" }}
            value={fields.bgFrom}
            onChange={(_, d) => set("bgFrom", d.value)}
          />
        </div>
        <div className={styles.field}>
          <span className={styles.label}>渐变止色 #RRGGBB</span>
          <Input
            size="small"
            aria-label="渐变止色"
            style={{ width: "120px" }}
            value={fields.bgTo}
            onChange={(_, d) => set("bgTo", d.value)}
          />
        </div>
      </div>
      <div className={styles.row}>
        {preview ? (
          <img className={styles.preview} src={preview} alt="美化预览" />
        ) : (
          <span className={styles.previewEmpty}>
            {busy === "preview" ? "生成预览…" : "尚无预览 · 点『预览』只看图，不落盘"}
          </span>
        )}
      </div>
      <div className={styles.ops}>
        <Button
          size="small"
          disabled={busy !== null}
          onClick={() => void runPreview(toBeautifySpec(fields))}
        >
          {busy === "preview" ? <Spinner size="tiny" /> : "预览"}
        </Button>
        <Button
          size="small"
          appearance="primary"
          disabled={busy !== null}
          onClick={() => void runExport()}
        >
          {busy === "export" ? <Spinner size="tiny" /> : target.label}
        </Button>
        <Button size="small" appearance="subtle" onClick={onClose}>
          取消
        </Button>
      </div>
    </div>
  );
}
