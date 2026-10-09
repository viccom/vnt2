import { defineStore } from "pinia";
import { ref, computed } from "vue";
import { getNotifications } from "../api";

const SEEN_KEY = "vnt-notify-seen";
// 平台通知轮询间隔：低频足够（服务端主动推送场景少，无需跟随 3s 实例轮询）
const POLL_MS = 15000;

let timer = null;

// 通知无服务端 ID，以（实例+时间+标题）组合为标识。同毫秒同标题的真重复
// 会被视为同一条（内容完全一致，可接受）——不能用单一时间戳水位：同毫秒
// 到达或本机时钟回拨的新通知会被漏标未读
const keyOf = (n) => `${n.file_name}|${n.received_at}|${n.title}`;

// 兼容历史水位格式与损坏数据：仅接受字符串数组
const loadSeen = () => {
  try {
    const value = JSON.parse(localStorage.getItem(SEEN_KEY) || "[]");
    return Array.isArray(value) ? new Set(value) : new Set();
  } catch {
    return new Set();
  }
};

export const useNotificationStore = defineStore("notifications", () => {
  const list = ref([]);
  const seen = ref(loadSeen());

  const unread = computed(
    () => list.value.filter((n) => !seen.value.has(keyOf(n))).length,
  );

  const persistSeen = () => {
    localStorage.setItem(SEEN_KEY, JSON.stringify([...seen.value]));
  };

  const refresh = async () => {
    try {
      list.value = (await getNotifications()) || [];
      // 服务端为环形缓冲（淘汰最旧/停止清空）：seen 收敛到当前列表，
      // 防止 localStorage 随历史通知无限增长
      const current = new Set(list.value.map(keyOf));
      const pruned = new Set([...seen.value].filter((k) => current.has(k)));
      if (pruned.size !== seen.value.size) {
        seen.value = pruned;
        persistSeen();
      }
    } catch {
      // 未登录/网络抖动静默跳过，不打扰角标场景
    }
  };

  const markSeen = () => {
    const next = new Set([...seen.value, ...list.value.map(keyOf)]);
    if (next.size !== seen.value.size) {
      seen.value = next;
      persistSeen();
    }
  };

  const start = () => {
    refresh();
    if (timer) return;
    timer = setInterval(() => {
      if (document.visibilityState === "visible") refresh();
    }, POLL_MS);
  };

  const stop = () => {
    if (timer) clearInterval(timer);
    timer = null;
  };

  return { list, seen, unread, refresh, markSeen, start, stop };
});
