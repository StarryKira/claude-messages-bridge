#!/usr/bin/env python3
"""Caller-owned tool execution using only Python's standard library."""
import json
import os
from urllib.request import Request, urlopen


def create_message(messages, tools):
    headers = {"content-type": "application/json", "anthropic-version": "2023-06-01"}
    if key := os.environ.get("BRIDGE_API_KEY"):
        headers["x-api-key"] = key
    request = Request(
        os.environ.get("BRIDGE_BASE_URL", "http://127.0.0.1:8787").rstrip("/") + "/v1/messages",
        headers=headers,
        data=json.dumps({
            "model": os.environ.get("MODEL", "claude-sonnet-4-6"),
            "max_tokens": 1024,
            "messages": messages,
            "tools": tools,
        }).encode(),
    )
    with urlopen(request, timeout=190) as response:
        return json.load(response)


def main():
    tools = [{
        "name": "weather",
        "description": "Return example weather data for a city. This is simulated data.",
        "input_schema": {
            "type": "object", "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    }]
    messages = [{"role": "user", "content": "调用 weather 查询上海和东京天气，注明这是示例数据。"}]
    for _ in range(8):
        response = create_message(messages, tools)
        for block in response["content"]:
            if block["type"] == "text":
                print(block["text"])
        if response["stop_reason"] != "tool_use":
            return
        messages.append({"role": "assistant", "content": response["content"]})
        results = []
        for block in response["content"]:
            if block["type"] != "tool_use":
                continue
            result = {"type": "tool_result", "tool_use_id": block["id"]}
            if block["name"] == "weather" and isinstance(block["input"].get("city"), str):
                result["content"] = json.dumps({
                    "city": block["input"]["city"], "condition": "sunny",
                    "temperature_c": 25, "simulated": True,
                }, ensure_ascii=False)
            else:
                result.update(content="Unknown tool or invalid city", is_error=True)
            results.append(result)
        messages.append({"role": "user", "content": results})
    raise RuntimeError("Tool roundtrip limit reached")


if __name__ == "__main__":
    main()
