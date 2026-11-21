import { create } from "axios";

const uriBuilder = create();

function serializeParams(params) {
  const search = new URLSearchParams();
  Object.entries(params ?? {}).forEach(([key, value]) => {
    if (value === null || value === undefined) return;
    if (Array.isArray(value)) {
      value.forEach((item) => {
        if (item !== null && item !== undefined) search.append(key, String(item));
      });
      return;
    }
    search.append(key, String(value));
  });
  return search.toString();
}

export function buildTestUrl({ baseURL, path = "/", params = {} } = {}) {
  return uriBuilder.getUri({
    baseURL,
    params,
    paramsSerializer: { serialize: serializeParams },
    url: path
  });
}

export function buildTestPath({ path = "/", params = {} } = {}) {
  return buildTestUrl({ params, path });
}
