# NylonME Integrations

LangChain 和 LlamaIndex 的 NylonME 适配器。核心共振逻辑只依赖
`nylon-sdk`，框架类在真正使用时才导入，避免强制安装框架。

## 安装

```bash
# LangChain
pip install nylonme-integrations[langchain]

# LlamaIndex
pip install nylonme-integrations[llamaindex]

# 两者都要
pip install nylonme-integrations[all]
```

## LangChain

```python
from nylonme_integrations.langchain import NylonMeRetriever, NylonMeMemory

retriever = NylonMeRetriever(target="127.0.0.1:50051", owner="alice", budget=5)
docs = retriever.invoke("上次出差住在哪里？")
for d in docs:
    print(d.page_content, d.metadata["resonance"])
```

对话记忆（在 prompt 里放一个 `{memory}` 占位符）：

```python
memory = NylonMeMemory(target="127.0.0.1:50051", owner="alice")
memory.save_context({"input": "我喜欢靠窗座位"}, {"output": "已记住"})
print(memory.load_memory_variables({"input": "出差选座"}))
# {'memory': '- 出差偏好：靠窗座位'}
```

## LlamaIndex

```python
from nylonme_integrations.llamaindex import NylonMeRetriever

retriever = NylonMeRetriever(target="127.0.0.1:50051", owner="alice", budget=5)
nodes = retriever.retrieve("上次出差住在哪里？")
for n in nodes:
    print(n.node.text, n.score)
```

## 配置

两个适配器都暴露同样的字段：

| 字段 | 默认值 | 说明 |
|---|---|---|
| `target` | `127.0.0.1:50051` | NylonME gRPC 地址（也读 `NYLON_SERVER`） |
| `owner` | `default` | 记忆归属（也读 `NYLON_OWNER`） |
| `tenant` | `default` | 租户（也读 `NYLON_TENANT`） |
| `budget` | `5` | 共振召回预算 |
| `task` | `None` | 可选任务上下文 |

引擎默认监听 `127.0.0.1:50051`（gRPC）和 `50052`（REST/UI）。用 Docker 一键起：
`docker compose up -d`。
