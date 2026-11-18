mkdir -p home/userdata
cat > home/userdata/settings.json <<'JSON'
{
  "defaultAutoPull": "yes",
  "branchNamePrefix": "odd",
  "keepAwakeWhileAgentsWork": 0,
  "sidebarAutoSettleAfterDays": 7.0,
  "futureSetting": {
    "level": 3,
    "deep": {
      "deeper": [
        1,
        2.0,
        {
          "x": null
        }
      ]
    }
  },
  "futureFlag": true,
  "storageCleanup": {
    "futureRule": 1,
    "logsAfterDays": "soon",
    "worktreeOnMerge": true
  },
  "observability": {
    "otlpTracesUrl": "http://otel.test",
    "futureUrl": "http://x.test"
  },
  "providerInstances": {
    "codex-ok": {
      "driver": "codex",
      "config": {
        "setupMode": "managed"
      },
      "futureField": 1
    },
    "bad": 5,
    "no-driver": {
      "displayName": "Nameless"
    }
  },
  "textGenerators": {
    "local": {
      "kind": "openaiCompatible",
      "baseUrl": "http://llm.test/v1"
    },
    "broken": {
      "kind": 7
    }
  },
  "usageLimitSources": {
    "hub": {
      "kind": "cliproxy",
      "url": "http://hub.test",
      "enabled": true
    },
    "broken": "x"
  },
  "snoozePresets": [
    {
      "kind": "after",
      "minutes": 5
    },
    {
      "kind": "until-reply",
      "from": "anyone"
    }
  ],
  "hiddenThreadTags": [
    "wip",
    7,
    "later"
  ],
  "favorites": [
    {
      "provider": "codex",
      "model": "a"
    },
    {
      "model": "no-provider"
    }
  ],
  "textGenerationModelSelection": {
    "model": "only-a-model"
  },
  "worktreesDirectory": 5,
  "previousWorktreesDirectories": "none",
  "defaultProjectScripts": [
    {
      "id": "setup",
      "name": "Setup",
      "command": "make deps",
      "icon": "configure",
      "runOnWorktreeCreate": true
    },
    {
      "name": "no command"
    }
  ],
  "projectSettingsOverrides": {
    "p1": {
      "defaultAutoPull": true,
      "futureOverride": 1
    },
    "p2": {
      "defaultAutoPull": "yes"
    }
  }
}
JSON
