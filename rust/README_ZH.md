# rust-agent

一个完整的、多文件的 Rust 参考智能体，面向 GOSIM Agent Observer Challenge
的协议（`participant-agent-protocol-v4`）。它在 stdin/stdout 上严格按参赛者
指南第 8 节的 JSON Lines 格式通信：先收到一条 `initialize`（无需回复），之后每
收到一条 `decision_request` 就回复恰好一条 `decision_response`，直到收到结束用
的 `finish`。

它只用协议公开的天球几何与计分公式来规划，思路上紧跟本项目同系列的 TypeScript
参考智能体（`examples/typescript`，同样是针对这套公开协议搭建和调过的）：
对候选指向做锚点搜索、按公开的观测夜日历睡过白天、维护一个自学习的天空质量
标量；在此之上每个观测夜还会让 LLM 微调几个旋钮（见下文"LLM 的使用"）。在
本地练习任务卡 `v4-practice-own/alpha` 上，跑出的分数比那个 TypeScript 基线
更高、耗时更短（见下文"自行验证"）。

启动需要一个 chat-completions API key，参见"LLM 的使用"。

English README: [README.md](README.md)。

## 它做了什么

1. 解析 `initialize.payload`（台址、仪器、计分配置、已公开的观测夜日历和目标
   表），存入 `state::Config` 这个带类型的快照：每个目标的可见时间窗
   （`max_hour_angle_deg`、首/末观测夜），以及一个按赤纬分带的空间索引，用来
   快速查"这附近还有哪些目标"。
2. 白天或两个观测夜之间，直接一步睡到下一个观测夜开始（`wait` 带 `until_utc`），
   而不是每隔一个曝光时长就 `wait` 一次去反复确认——观测夜日历本身是公开的，
   查得更频繁也不会多知道什么。
3. 在夜里，对仍可见、尚未完成的目标打分排序（`planner::value`）：还没"安全"
   达标的 `required` 目标会拿到一个加成，因为结算时漏掉一个是要扣真金白银的
   分。对打分最高的几个候选，会**把它放在每一个光纤位置上试一遍**作为指向锚点
   （`scoring::shift_altaz`），再用视场里其它落在各自光纤玻璃上的目标填满剩下
   的光纤，最后保留总期望价值最高的那个指向——而不是只盯着单个最佳目标自己
   所在的那个指向。
4. 在十个候选曝光时长里挑出单位时间期望收益最高的一个，并选出大多数被指派
   目标预期会落入的 program（`DARK`/`BRIGHT`/`BACKUP`）。
5. 用自己实际收到的 `last_result.hits` 得分学习一个天空质量标量
   （`memory::Memory::scale`，取最近若干样本的中位数）——这正是指南建议的、
   绕开隐藏的逐 slot 天气真值的办法——并且从实际拿到的"声明档位 vs 不匹配"
   倍率反推出每次命中真正对应的完成因子。
6. 每个观测夜向 LLM 发出两次有严格限界的询问（见下文"LLM 的使用"）。
7. 每条即将发出的响应都先过 `validate.rs` 的协议硬规则检查；一旦自己的计划有
   任何问题，就换成一个保证合法的最短 `wait`，而不是冒险发出可能被判
   `agent_error` 的动作。

## 模块结构

| 文件 | 职责 |
|---|---|
| `src/main.rs` | 读取 `initialize`，驱动决策循环；日志写到 stderr。 |
| `src/protocol.rs` | 协议消息的 serde 类型；宽容式 JSON Lines 读写。 |
| `src/state.rs` | 来自 `initialize` 的一次性配置快照：目标表、观测夜日历、可见时间窗、空间索引。 |
| `src/memory.rs` | 智能体从自己的反馈中学到的东西（进度、学习到的标量、天气提示），以及 stderr 日志函数。 |
| `src/planner.rs` | 把一条 `decision_request` 变成一条 `decision_response`：锚点搜索与曝光时长/program 选择。 |
| `src/llm.rs` | OpenAI 兼容 chat 客户端（默认走 Kimi Coding Plan），带重试与整次运行的用量上限。 |
| `src/scoring.rs` | 公开的天球几何与计分公式（不含任何场景数据）。 |
| `src/validate.rs` | 协议规则校验，以及确定性的安全兜底动作。 |

## 构建与运行

```sh
cargo build --release
OPENAI_API_KEY=<你的 key> ./target/release/rust-agent < some_session.jsonl
```

构建不需要任何场景数据，也不需要 key。运行需要 key——来源和不设置时的行为见
下文"LLM 的使用"。运行起来之后，智能体看到的全部内容就是 stdin 上实际收到的
`initialize`/`decision_request` 消息；本示例不附带任何场景数据。

### 打包提交

`observer.project.json` 告诉平台如何构建和运行本项目：

```json
{
  "build": [["cargo", "build", "--release", "--locked"]],
  "run": ["./target/release/rust-agent"]
}
```

构建耗时不计入观测周期的实际运行时间（该时钟从第一条 `decision_request` 才开
始计），因此 release profile 选择了编译更快的 `opt-level = 2`，而不是追求极限
优化。

### 自行验证

把任意支持该协议的本地测试工具（平台本身，或你自己的测试工具）指向编译好的
二进制即可——它接受一个普通的命令数组作为智能体进程，驱动一个编译好的二
进制同样可以。把 `OPENAI_BASE_URL` 指向本地搭的 OpenAI 兼容 mock（或填真实
key），就能在没有真实服务商参与的情况下对着公开练习任务卡跑一遍。

## LLM 的使用

配置方式（参见 `.env.example`）：

- **Key**：`OPENAI_API_KEY`；如果只有 Kimi 的 key，也可以用 `KIMI_API_KEY`
  这个名字。两个都没设置时，智能体会打印
  `missing API key: set OPENAI_API_KEY` 并退出，不会去读 stdin 上的任何内容。
- **端点/模型**：默认是 Kimi Coding Plan
  （https://www.kimi.com/code/docs/en/），一个 OpenAI 兼容的 chat-completions
  接口：base URL 为 `https://api.kimi.com/coding/v1`，模型为 `k3`。设置
  `OPENAI_BASE_URL` / `OPENAI_MODEL` 可以改用该服务的海外地址
  （`https://api.kimi.ai/coding/v1`），或者任何其它 OpenAI 兼容服务商。

每个观测夜都会发出两次调用，结果会合并（规避方位取并集，曝光时长缩放取
平均）：

1. **夜间建议**（`llm::ask_night_advice`）：读取为今晚记录的公开预报提示和
   当前公告，询问要规避哪些方位、曝光时长要不要缩放（0.7-1.4）。
2. **命中率建议**（`llm::ask_hitrate_advice`）：读取同一晚的实时公告，加上
   智能体自己到目前为止的命中率，问的是同样两件事。

这两次调用只会**微调**计划本身的曝光时长和要规避的方位，从不负责选择指向、
光纤指派或声明的 program。一次调用失败（连接/超时错误、回复不是 JSON、回复
解析不出期望的字段）会重试最多 3 次；如果全部失败，当晚这一步就用计划自己
的默认数值，下一晚的调用照常再试一遍。

上限（均可通过 `.env` 覆盖）：

- 每次尝试 `LLM_TIMEOUT_SECONDS`（默认 12 秒）。
- 整次运行调用实际耗时总预算 `LLM_BUDGET_SECONDS`（默认 300 秒）。
- 整次运行调用次数上限 `LLM_MAX_CALLS`（默认 100 次）。
- 一旦运行时间预算剩余不足 30 秒，规划器就不再尝试调用 LLM；剩余不足 10 秒时
  主动发送 `finish`，而不是冒险在决策过程中被直接杀掉。

## 关于确定性

有两处内部用的 map 特意选了 `BTreeMap` 而不是 `HashMap`：一处是单次指向视场内
的光纤指派，另一处是在两次决策之间"挂起等结果"的目标集合。原因是 `HashMap`
的遍历顺序在每个进程里是随机的，一旦两个候选打平，这个随机性就会悄悄决定规
划器留下哪一个——同一个二进制跑同一张任务卡，不同的进程运行会跑出实打实不
同的轨迹和分数，这还是在 LLM 回复本身尚未带来任何差异之前。修掉这一点之后，
确定性的核心部分（几何、计分、光纤搜索）对同样的输入永远做同样的选择——同
一张任务卡、同样的 LLM 回复，每次都是同一个分数。

## 安全说明

本示例**不包含任何场景数据**：没有 truth 文件，没有 `v4_bulletins.jsonl` /
`v4_forecasts.jsonl`，没有天气数值表，也没有从任何任务卡的 `public/`、
`config/` 或 `truth/` 目录复制任何内容。智能体唯一会看到的公告/预报，就是真实
协议在运行时通过 `decision_request.payload.new_messages` 送达的那些。可以自行
验证：

```sh
grep -rniE "truth|bulletin|forecasts\.jsonl" src/ | grep -v record_type
```

（剩下命中的都是注释/提示词里提到协议实际发送的 `bulletin`/`forecast` *消息
类型*,以及强调"从不读取任何 truth 文件"这件事本身——不是任何打包进来的数据。）

## 已知局限

- 锚点搜索每次决策只尝试有限数量的候选目标和光纤位置
  （`planner::ANCHORS`、`ANCHOR_POOL`），不是穷举——面对一万多个目标时这是
  合理的取舍，但不是最优解。
- 恒星时、日月位置用的是指南本身给出的低阶近似公式（精度到几个角秒量级），
  不是完整的历表计算。
- 主动 `report` 的触发条件（`planner::maybe_report`）刻意设得很保守（需要连续
  一长串满光纤指派却零命中），在容易的任务卡上可能永远不会触发——这是有意
  为之,因为猜错要付出 `scoring.reporting.false_penalty` 的代价。

## 许可 / 引用

任务卡、模拟数据、评测代码与本示例项目按 [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/)
提供（署名、非商业）；使用请引用 GOSIM 2026 Agentic Observer Challenge。选手自己编写的代码不受此限制。
详见 `LICENSE.md`。
