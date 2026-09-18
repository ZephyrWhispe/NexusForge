// D-17 前端门禁：ESLint flat config（typescript-eslint + react-hooks + jsx-a11y）。
// 目标"eslint 零告警"（UI-PLAN §4）；react-refresh 规则刻意不启用（Vite 自带 HMR 提示已足够）。
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import jsxA11y from "eslint-plugin-jsx-a11y";

export default tseslint.config(
  { ignores: ["dist/", "target/", "node_modules/", "src-tauri/target/", "demo/"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  reactHooks.configs.flat["recommended-latest"],
  jsxA11y.flatConfigs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    rules: {
      // D-19 兜底：禁止裸 catch 吞错，全库统一走 reportError（stores/notifications.ts）
      "no-empty": ["error", { allowEmptyCatch: false }],
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      // react-hooks v7 随附的 React-Compiler 系规则超出 D-17 裁决范围
      // （D-17 明确只要 exhaustive-deps + 键盘可达类 a11y 缺陷）；
      // set-state-in-effect 等会把常规"effect 内取数后 setState"全判错，留待独立立项。
      "react-hooks/set-state-in-effect": "off",
      "react-hooks/purity": "off",
      "react-hooks/immutability": "off",
      "react-hooks/refs": "off",
      "react-hooks/incompatible-library": "off",
      "react-hooks/preserve-manual-memoization": "off",
    },
  },
  {
    // 构建脚本跑在 Node：浏览器全局之外另开 Node 作用域
    files: ["tools/**/*.mjs", "eslint.config.js", "vite.config.ts"],
    languageOptions: { globals: { console: "readonly", process: "readonly" } },
  },
);
