"""LlamaIndex retrieval over NylonME memories."""

from nylonme_integrations.llamaindex import NylonMeRetriever

TARGET = "127.0.0.1:50051"
OWNER = "alice"


def main() -> None:
    retriever = NylonMeRetriever(target=TARGET, owner=OWNER, budget=5)

    for query in ("用户差旅偏好", "上次出差去了哪里"):
        print(f"\nQ: {query}")
        for node in retriever.retrieve(query):
            print(f"  [{node.score:.2f}] {node.node.text}")


if __name__ == "__main__":
    main()
