from __future__ import annotations

import json
import re
import urllib.parse
import urllib.request
import uuid
from dataclasses import dataclass

from .config import BILIBILI_HEADERS
from .models import PlaylistItem

VIDEO_PATH_RE = re.compile(r"/video/(?P<vid>(BV[0-9A-Za-z]+|av\d+))", re.IGNORECASE)
BV_RE = re.compile(r"^(BV[0-9A-Za-z]+)$", re.IGNORECASE)
AV_RE = re.compile(r"^(av\d+)$", re.IGNORECASE)
SHORT_HOSTS = {"b23.tv", "bili2233.cn"}


@dataclass
class VideoReference:
    original_url: str
    resolved_url: str
    bvid: str = ""
    aid: int = 0
    page: int = 1


class BilibiliError(RuntimeError):
    pass


def request_json(url: str) -> dict:
    request = urllib.request.Request(url, headers=BILIBILI_HEADERS)
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.loads(response.read().decode("utf-8"))


def resolve_video_reference(raw_input: str) -> VideoReference:
    cleaned = raw_input.strip()
    if not cleaned:
        raise BilibiliError("请输入 B 站视频链接")

    if BV_RE.match(cleaned):
        cleaned = f"https://www.bilibili.com/video/{cleaned}"
    elif AV_RE.match(cleaned):
        cleaned = f"https://www.bilibili.com/video/{cleaned}"
    elif not cleaned.startswith(("http://", "https://")):
        cleaned = f"https://{cleaned}"

    resolved_url = cleaned
    parsed = urllib.parse.urlparse(cleaned)
    if parsed.netloc.lower() in SHORT_HOSTS:
        request = urllib.request.Request(cleaned, headers=BILIBILI_HEADERS, method="GET")
        with urllib.request.urlopen(request, timeout=15) as response:
            resolved_url = response.geturl()

    parsed = urllib.parse.urlparse(resolved_url)
    match = VIDEO_PATH_RE.search(parsed.path)
    if not match:
        raise BilibiliError("当前仅支持普通 B 站视频 URL 或 BV/av 号")

    raw_vid = match.group("vid")
    query = urllib.parse.parse_qs(parsed.query)
    page = int(query.get("p", ["1"])[0] or "1")
    if raw_vid.lower().startswith("bv"):
        return VideoReference(
            original_url=cleaned,
            resolved_url=resolved_url,
            bvid=raw_vid,
            page=max(page, 1),
        )
    return VideoReference(
        original_url=cleaned,
        resolved_url=resolved_url,
        aid=int(raw_vid[2:]),
        page=max(page, 1),
    )


def fetch_video_item(raw_input: str) -> PlaylistItem:
    reference = resolve_video_reference(raw_input)
    if reference.bvid:
        api_url = (
            "https://api.bilibili.com/x/web-interface/view?"
            f"bvid={urllib.parse.quote(reference.bvid)}"
        )
    else:
        api_url = (
            "https://api.bilibili.com/x/web-interface/view?"
            f"aid={reference.aid}"
        )

    payload = request_json(api_url)
    if payload.get("code") != 0:
        message = payload.get("message") or "获取视频信息失败"
        raise BilibiliError(message)

    data = payload["data"]
    pages = data.get("pages") or []
    if not pages:
        raise BilibiliError("视频没有可播放的分 P 信息")

    page_number = min(reference.page, len(pages))
    page_info = pages[page_number - 1]
    aid = int(data["aid"])
    bvid = str(data["bvid"])
    cid = int(page_info["cid"])
    title = str(data.get("title") or "").strip()
    part_title = str(page_info.get("part") or f"P{page_number}").strip()
    display_title = f"{title} - {part_title}"
    embed_query = urllib.parse.urlencode(
        {
            "aid": aid,
            "bvid": bvid,
            "cid": cid,
            "page": page_number,
            "high_quality": 1,
            "danmaku": 0,
            "autoplay": 1,
            "isOutside": "true",
        }
    )
    embed_url = f"https://player.bilibili.com/player.html?{embed_query}"

    return PlaylistItem(
        id=uuid.uuid4().hex[:12],
        original_url=reference.original_url,
        resolved_url=reference.resolved_url,
        bvid=bvid,
        aid=aid,
        cid=cid,
        page=page_number,
        title=title,
        part_title=part_title,
        display_title=display_title,
        cover_url=str(data.get("pic") or ""),
        embed_url=embed_url,
    )
