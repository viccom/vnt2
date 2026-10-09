import { defineStore } from "pinia";
import { ref, computed } from "vue";
import { getNotifications } from "../api";

const SEEN_KEY = "vnt-notify-seen";
// 平台通知轮询间隔：低频足够（服务端主动推送场景少，无需跟随 3s 实例轮询）
const POLL_MS = 15000;

let timer = null;

export const useNotificationStore = defineStore("notifications", () => {
  const list = ref([]);
  // 已读水位：只认通知自身的 received_at，避免本机时钟偏移误标
  const lastSeenAt = ref(Number(localStorage.getItem(SEEN_KEY)) || 0);

  const unread = computed(
    () => list.value.filter((n) => n.received_at > lastSeenAt.value).length,
  );

  const refresh = async () => {
    try {
      list.value = (await getNotifications()) || [];
    } catch {
      // 未登录/网络抖动静默跳过，不打扰角标场景
    }
  };

  // 打开面板即视为已读：水位推进到当前列表最新一条
  const markSeen = () => {
    const newest = list.value.reduce((max, n) => Math.max(max, n.received_at), 0);
    if (newest > lastSeenAt.value) {
      lastSeenAt.value = newest;
      localStorage.setItem(SEEN_KEY, String(newest));
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

  return { list, lastSeenAt, unread, refresh, markSeen, start, stop };
});
