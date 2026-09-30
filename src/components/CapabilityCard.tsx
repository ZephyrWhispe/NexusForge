import { makeStyles, Text, tokens } from "@fluentui/react-components";
import { ChevronDownRegular, ChevronRightRegular } from "@fluentui/react-icons";
import { useState } from "react";

import DeferredBadge from "./DeferredBadge";
import { SPACING, TIER_W } from "./nfTiers";
import type { ModuleCapabilities } from "../layout/capabilities";

/**
 * 面板内能力卡（D-43 ③）：一行常驻说"这个面板能做什么"，展开给能做／尚不做／上限与截断／快捷键。
 *
 * 三条纪律写在这里而不是注释里措辞：
 *  · 默认折叠且**首帧永不自动展开**（用户明令"不要首页引导"——无引导、无自动弹出、无向导）；
 *  · 无 detail 的面板只渲一行文本，连展开钮都不给（未经核读的清单＝装完成）；
 *  · 尚不做一律经既有 DeferredBadge 显影，不在这里新建第二种延后徽标语义。
 */

const useStyles = makeStyles({
  root: {
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    color: tokens.colorNeutralForeground3,
    display: "flex",
    flexDirection: "column",
    gap: SPACING.x8,
    paddingBottom: SPACING.x8,
  },
  bar: {
    alignItems: "center",
    background: "transparent",
    border: "none",
    color: "inherit",
    cursor: "pointer",
    display: "flex",
    font: "inherit",
    gap: SPACING.x8,
    minHeight: SPACING.x24,
    padding: 0,
    textAlign: "left",
  },
  barStatic: {
    alignItems: "center",
    display: "flex",
    gap: SPACING.x8,
    minHeight: SPACING.x24,
  },
  icon: { flexShrink: 0 },
  line: {
    flex: 1,
    minWidth: 0,
  },
  body: {
    display: "grid",
    gap: SPACING.x16,
    // 展开区两栏（窄容器自动回落一栏）：四段竖排实测高 464px，会把面板压到 172px 可用高，
    // 与"保持高密度"裁决相背；分栏后同一批事实只占一半竖向空间。
    gridTemplateColumns: `repeat(auto-fit, minmax(${TIER_W.m}, 1fr))`,
    paddingBottom: SPACING.x4,
  },
  sec: {
    display: "flex",
    flexDirection: "column",
    gap: SPACING.x4,
    minWidth: 0,
  },
  seg: {
    color: tokens.colorNeutralForeground1,
    fontSize: tokens.fontSizeBase200,
    lineHeight: tokens.lineHeightBase200,
  },
  segHead: {
    color: tokens.colorNeutralForeground3,
  },
  chip: {
    borderBottom: `1px solid ${tokens.colorNeutralStroke2}`,
    borderLeft: `3px solid ${tokens.colorBrandStroke1}`,
    borderRadius: tokens.borderRadiusSmall,
    display: "inline",
    fontFamily: tokens.fontFamilyMonospace,
    fontSize: tokens.fontSizeBase100,
    marginRight: SPACING.x8,
    padding: `0 ${SPACING.x4}`,
  },
});

export default function CapabilityCard({ capabilities }: { capabilities: ModuleCapabilities }) {
  const styles = useStyles();
  const [open, setOpen] = useState(false);
  const { collapsed, detail } = capabilities;

  if (!detail) {
    return (
      <div className={styles.root} data-nf="capability">
        <div className={styles.barStatic}>
          <Text size={200} className={styles.line}>
            {collapsed}
          </Text>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.root} data-nf="capability">
      <button
        type="button"
        className={styles.bar}
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {open ? (
          <ChevronDownRegular className={styles.icon} aria-hidden />
        ) : (
          <ChevronRightRegular className={styles.icon} aria-hidden />
        )}
        <Text size={200} className={styles.line}>
          {collapsed}
        </Text>
        <Text size={200} className={styles.icon}>
          {open ? "收起" : "能做到什么程度"}
        </Text>
      </button>
      {open && (
        <div className={styles.body}>
          <div className={styles.sec}>
            <Text size={200} weight="semibold" className={styles.segHead}>
              能做
            </Text>
            {detail.can.map((s) => (
              <Text key={s} size={200} className={styles.seg}>
                {s}
              </Text>
            ))}
          </div>
          {detail.notYet.length > 0 && (
            <div className={styles.sec}>
              <Text size={200} weight="semibold" className={styles.segHead}>
                尚不做
              </Text>
              <div className={styles.seg}>
                {detail.notYet.map((s) => (
                  <DeferredBadge key={s} label={s} decisionRef="D-43 明确不做" />
                ))}
              </div>
            </div>
          )}
          <div className={styles.sec}>
            <Text size={200} weight="semibold" className={styles.segHead}>
              上限与截断
            </Text>
            {detail.limits.map((s) => (
              <Text key={s} size={200} className={styles.seg}>
                {s}
              </Text>
            ))}
          </div>
          <div className={styles.sec}>
            <Text size={200} weight="semibold" className={styles.segHead}>
              快捷键
            </Text>
            {detail.keys.map((s) => {
              const i = s.indexOf("：");
              const key = i < 0 ? s : s.slice(0, i);
              const what = i < 0 ? "" : s.slice(i + 1);
              return (
                <span key={s} className={styles.seg}>
                  <kbd className={styles.chip}>{key}</kbd>
                  {what}
                </span>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}
