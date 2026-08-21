import { afterEach, describe, expect, it } from "vitest";
import { createDocument } from "@vcad/ir";
import { useDocumentStore } from "../stores/document-store.js";

const originalState = useDocumentStore.getState();

afterEach(() => {
  useDocumentStore.setState(originalState, true);
});

describe("preserved Loon source", () => {
  it("is invalidated when a geometry mutation materializes a new document", () => {
    const document = createDocument();
    const engine = {
      __wbg_ptr: 1,
      add_feature: () => ({
        document,
        parts: [],
        consumedPartIds: [],
        createdFeatureId: "feature-1",
      }),
    };

    useDocumentStore.setState({
      loonSource: "[cube 20.0 20.0 20.0]",
      _crdtEngine: engine as never,
    });

    useDocumentStore.getState().addPrimitive("cube");

    expect(useDocumentStore.getState().loonSource).toBeNull();
  });
});
