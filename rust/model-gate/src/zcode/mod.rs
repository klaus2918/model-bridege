//! ZCode 原生插件：账号存储、凭据、OAuth、领取、验证码与 GLM transport。
//!
//! 本模块是 Model-Gate 内部模块，不是第二套网关/账号库/UI。详细设计见
//! `.op/changes/free-source-management/plan.md`。

pub mod api;
pub mod appserver;
pub mod billing;
pub mod captcha;
pub mod claim;
pub mod client;
pub mod config;
pub mod credentials;
pub mod oauth;
pub mod secret;
pub mod service;
pub mod store;
pub mod transport;
