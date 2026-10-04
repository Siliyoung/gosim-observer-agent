# GOSIM Agent Observer Challenge —— TypeScript（Node.js）示例智能体

这是面向 `participant-agent-protocol-v4` 协议的完整 TypeScript 示例智能体，依赖极少：它模拟运行一台
16 光纤光谱巡天望远镜，仅依据公开信息决定指向、光纤—目标分配、曝光时长和观测程序（program）。

本项目与 Python 版示例并列：协议、评分规则、打包方式完全一致，只是用 Node.js 的惯用写法重新实现
了一遍。完整协议与评分公式请参阅本项目的 `docs/`；本文件只说明这个 TypeScript 项目特有的部分。

English version: [`README.md`](./README.md).

## 目录结构

```
src/
  protocol.ts    JSON Lines 传输层：消息类型定义、信封编解码、stdout/stderr 纪律
  skymath.ts      天球几何：地方恒星时、高度角/方位角、日月位置、光纤网格投影
  scoring.ts      仅用公开评分配置实现的评分辅助函数（曝光质量、program 档位判定）
  state.ts        智能体状态：目标目录、学习到的天空质量、各目标完成进度、消息处理
  planner.ts      决策逻辑：选指向、填光纤、选曝光时长与 program
  memory.ts       运行期滚动记录：stderr 进度日志 + 供大模型使用的精简上下文
  llmClient.ts    OpenAI 兼容对话客户端（用 Node 内置 fetch 实现，Kimi 等同样可用）
  validate.ts     依据公开限制校验动作 + 兜底的确定性动作
  index.ts        入口：读取 stdin，分发 initialize / decision_request / finish
observer.project.json   平台识别的项目清单（见下文“提交”一节）
.env.example             本地试用大模型钩子的环境变量模板
```

只使用了两个 npm 包，且都是开发期依赖：`typescript`（构建用）和 `@types/node`（为内置的
`fetch`/`process` 等提供类型声明）。运行时没有任何第三方依赖。

## 快速开始

```bash
npm install
npm run build        # tsc -> dist/
npm start             # node dist/index.js（从 stdin 读取 JSON Lines，写到 stdout）
```

本项目不自带本地测试工具；把任意支持该协议的 JSON Lines 测试工具（平台本身，或你自己的引擎适配器）
指向 `node dist/index.js`、工作目录设为本项目根目录即可。智能体只在 stdin/stdout 上使用协议，日志
全部写到 stderr，这一点与 Python 示例完全一致。

已用平台自带的引擎适配器对一张 10,000 目标、38 晚的练习任务卡做过端到端验证：几秒钟内（预算为
900 秒）跑完，无协议错误，总分为正，约 96% 的 `required` 目标被完成。

## 各模块职责

- **protocol.ts**：唯一接触 stdin/stdout 的文件。定义文档中出现的全部消息结构（`initialize`、
  `decision_request`、`finish`，以及我们发出的 `decision_response`），并确保每条响应只带上该动作
  真正需要的字段（多余字段会导致任务卡终止）。
- **skymath.ts**：对引擎自身公式的直接、零依赖移植（地方恒星时、赤道/地平坐标互转、低精度日月
  历表、光纤网格的球面心射投影），确保这里规划出的指向和引擎判定的命中结果一致。
- **scoring.ts**：只用 `initialize.payload.scoring` 中的公开配置重建的评分子集（月光因子、完成
  因子、program 档位阈值），从不依赖隐藏真值。
- **state.ts**：保存目标目录，以及智能体在运行中学到的一切：根据自身曝光结果估计的天空质量、
  各目标的最佳完成因子、公告/预报、地形与事件提示，以及 Hard mode 下仅可能出现一次的
  `state_resync` 恢复流程。
- **planner.ts**：每次决策时，对可见且未完成的目标排序（尚未安全达标的 `required` 目标会获得
  额外权重），围绕排名靠前的若干候选目标尝试多种指向，为全部 16 根光纤各自填入落在其方格内、
  价值最高的目标，再挑选单位时间期望得分最高的曝光时长与 program。
- **memory.ts**：轻量的滚动计数器（命中率、已见预报），既用于 stderr 进度日志，也用于为大模型
  拼装精简、且只含公开信息的上下文。
- **llmClient.ts**：基于 Node 内置 `fetch`（Node ≥ 18）实现的轻量 OpenAI 兼容对话客户端，默认对接
  Kimi Coding Plan，也可以通过环境变量指向 OpenAI、平台自带的模型代理，或其他任何兼容
  `/chat/completions` 的服务。每次调用都有超时，整个运行还有调用预算上限；单次调用失败或超时会
  重试有限次数，回复格式错误时解析结果为 `null`，绝不抛出异常。
- **validate.ts**：在发出之前，依据公开限制（高度角/方位角范围、曝光时长边界、光纤/目标是否
  重复、连续 `report` 上限）校验每一个动作（无论来自规划器还是大模型）；校验失败的动作会被替换
  为安全的 `wait`/`finish`，不会被发送到 stdout。
- **index.ts**：把以上模块串起来。白天用一次带 `until_utc` 的 `wait` 睡过去；公告称全天区有雨或
  风暴时关闭快门等一个 slot；每晚调用大模型两次获取避让方向和曝光时长建议；在规则已怀疑仪器
  故障时，调用大模型确认是否要提交 `report`（每次运行最多举报两次）；其余情况交给规划器处理。
  所有分支都做了保护，内部错误绝不会让整个运行崩溃，而是退回一次安全的 `wait`。

### 大模型的使用位置

1. **夜间规划**（`llmClient.nightPlan`，由 `index.ts` 的 `nightAdvice` 调用）：每晚一次，把当晚的
   预报提示和当前公告交给模型，请它给出需要避开的方位和一个曝光时长缩放系数。
2. **公告实时核查**（`llmClient.bulletinCheckIn`，紧接着在 `nightAdvice` 中调用）：每晚另外
   单独一次调用，把当前公告和这次运行至今的命中率交给模型,再次请它给出需要避开的方位和一个
   曝光时长缩放系数——这一次是对眼下情况的反应,而不是预报。两次结果会合并(避让方位取并集,
   缩放系数取平均),共同影响当晚剩余时间的指向选择和曝光长度。
3. **故障举报确认**（`llmClient.confirmReport`，由 `index.ts` 的 `maybeReport` 调用）：当
   `state.ts` 中的规则判断（`faultEvidence`）已经发现跨多晚、持续且无法用公告解释的质量下降时，
   请模型确认或否决本次 `report`（每次运行至多举报两次；错误举报会扣分）。这一步是有条件的——
   只有出现这种模式时才会触发。

调用都设有上限（默认单次 12 秒超时、总预算 300 秒、至多 100 次调用）；单次调用失败或超时会重试
有限次数，仍不成功时，这一步就改用规则判断的结果。

## 大模型配置

客户端对接任意兼容 OpenAI `/chat/completions` 的服务，默认使用 Kimi Coding Plan
（参见 https://www.kimi.com/code/docs/en/ ）：

- `OPENAI_BASE_URL` —— 默认 `https://api.kimi.com/coding/v1`（海外可用
  `https://api.kimi.ai/coding/v1`）。
- `OPENAI_API_KEY` —— 该服务的密钥（也接受 `KIMI_API_KEY`）。
- `OPENAI_MODEL` —— 默认 `k3`。

在平台上，这些变量会在每次运行时自动注入，指向平台自己的模型代理和一个临时凭据。本地试用方法：

```bash
cp .env.example .env     # 然后编辑 OPENAI_BASE_URL / OPENAI_API_KEY / OPENAI_MODEL
```

`node dist/index.js` 不会自动读取 `.env`；运行测试工具前，先在 shell 里导出这些变量，或者 source
一下：

```bash
set -a && source .env && set +a && node dist/index.js < some_transcript.jsonl
```

**切勿**提交真实的 `.env` 或把它打进提交 ZIP——平台会直接拒绝含 `.env` 的包。

## 提交方式

1. 本地先跑一次 `npm run build`，确认 `dist/` 能编译通过（平台会按 `observer.project.json` 里的
   `build` 步骤自己构建一次；不需要把 `dist/` 提交进去）。
2. 在 Participate 页面把本文件夹打包上传（或推送到一个 GitHub 仓库）。根目录的
   `observer.project.json` 已经声明好：
   - `"image": "node:20-slim"` —— 直接使用标准公开镜像拉取 Node，不涉及任何 Python；
   - `"build": [["npm", "ci"], ["npm", "run", "build"]]` —— 安装两个开发期依赖并把 TypeScript
     编译到 `dist/`；
   - `"run": ["node", "dist/index.js"]` —— 运行编译产物；
   - `"protocol": "jsonl-v4"` —— 与 Python 示例相同的 JSON Lines 传输协议。
3. 如果你添加了运行时依赖，请写进 `package.json` 的 `dependencies`（而不是 `devDependencies`——
   `npm ci` 两者都会装，但分开写能如实说明哪些是运行必需的）。

## 安全说明

本示例刻意不包含任何场景数据：没有 `targets.csv`/`footprint.csv`，没有 `truth/` 目录，也没有
`v4_bulletins.jsonl`/`v4_forecasts.jsonl`。智能体对本次运行的全部了解都来自 `initialize` 消息和
运行期经 stdin 实时收到的 `new_messages`，与它在平台上的处境完全一致。它只会执行文档中列出的四种
动作（`observe`、`wait`、`report`、`finish`），也只读取协议交给它的内容。

## 许可 / 引用

任务卡、模拟数据、评测代码与本示例项目按 [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/)
提供（署名、非商业）；使用请引用 GOSIM 2026 Agentic Observer Challenge。选手自己编写的代码不受此限制。
详见 `LICENSE.md`。
