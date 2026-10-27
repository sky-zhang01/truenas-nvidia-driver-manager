# 安全政策

[English](SECURITY.md)

## 支持版本与环境

安全修复面向最新发布的版本。首个公开版本为 **0.2.0**，更早的开发版本不在支持范围。支持并实测的平台为 **TrueNAS SCALE 25.10.7 与 TrueNAS 27.0.0-RC.1 / amd64**，其他 TrueNAS 版本和架构均未测试。请使用最新支持版本，并阅读 [安全说明](docs/security.zh-CN.md)。

App 使用特权及主机 PID 访问能力，可替换主机内核驱动代码，适合可信管理局域网。程序提供 HTTP，会话 cookie 不含 `Secure`；HTTPS 及所需的 cookie 重写由部署侧负责。

## 私下报告

请使用本仓库 **Security → Advisories → Report a vulnerability** 的[私有漏洞报告入口](https://github.com/sky-zhang01/truenas-nvidia-driver-manager/security/advisories/new)。未修复漏洞及利用细节请勿放在公开 issue 或 PR 中。

请附应用版本或镜像摘要、TrueNAS 版本、受影响组件、影响、前提条件、复现步骤或精简概念验证。日志和附件须去除口令、cookie、`X-NVM-Token`、私钥、主机地址及无关个人数据。只在自己拥有或获准评估的系统上测试。

维护者目标是在 **7 天**内确认收到，**30 天**内提供初步判断或进展更新。本项目由志愿者维护，这些是目标，不是响应或修复时限保证。修复评估期间，请在私有 advisory 中协商披露。

## 范围

本项目代码、认证、API、上传处理、构建编排及分发容器镜像的安全问题属于范围。TrueNAS 或 NVIDIA 软件本身的漏洞请报对应厂商，本项目无法为这些产品发布修复。边界不清时，可在私有报告中描述交互。普通使用问题及非安全缺陷请在脱敏后提交公开 issue。
