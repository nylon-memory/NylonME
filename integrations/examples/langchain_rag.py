"""LangChain RAG over NylonME memories.

Run once to weave a few facts, then ask questions that are answered from
memory instead of a document store.
"""

from nylonme_integrations.langchain import NylonMeRetriever

TARGET = "127.0.0.1:50051"
OWNER = "alice"


def main() -> None:
    retriever = NylonMeRetriever(target=TARGET, owner=OWNER, budget=5)

    for query in ("用户差旅偏好", "上次出差去了哪里"):
        print(f"\nQ: {query}")
        for doc in retriever.invoke(query):
            print(f"  [{doc.metadata['resonance']:.2f}] {doc.page_content}")


if __name__ == "__main__":
    main()
