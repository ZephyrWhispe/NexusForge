/**
 * 全局测试桩（PERF-04 配套）：jsdom 无 ResizeObserver/真实滚动矩形，
 * useVirtualizer 观测不到容器就不产行。默认用"直通假实现"按 count 全量展开
 * 虚拟行（与各面板既有本地 stub 同形）；需要定制虚拟行为的测试文件可用
 * vi.mock("@tanstack/react-virtual", ...) 就地覆盖本桩。
 */
import { vi } from "vitest";

vi.mock("@tanstack/react-virtual", () => ({
  useVirtualizer: ({ count }: { count: number }) => ({
    getTotalSize: () => count * 32,
    getVirtualItems: () =>
      Array.from({ length: count }, (_, i) => ({
        index: i,
        start: i * 32,
        end: (i + 1) * 32,
        size: 32,
        key: i,
      })),
    measureElement: () => {},
    scrollToIndex: () => {},
  }),
}));
