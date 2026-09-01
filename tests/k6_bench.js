// k6 benchmark script
// Port from env K6_TARGET_PORT (default 18091)
// Comparable with loadforge_bench.py

import http from "k6/http";
import { check } from "k6";

export const options = {
  vus: 100,
  duration: "10s",
};

const PORT = __ENV.K6_TARGET_PORT || "18091";
const BASE_URL = `http://127.0.0.1:${PORT}`;

export default function () {
  const res = http.get(`${BASE_URL}/api/test`);
  check(res, { "status is 200": (r) => r.status === 200 });
}
