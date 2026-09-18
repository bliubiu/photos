//! 进程级推理引擎池：任务执行时借出引擎、结束后归还，避免每个任务重复装载 ONNX 会话
//! （模型装载是批量处理的主要固定开销）。
//!
//! 池容量取 `[server] max_concurrent_tasks`：并发任务各持一个引擎互不阻塞，空闲引擎留在
//! 池中复用（已装载的模型会话常驻，含按需加载的换装解析模型）。
//!
//! 空闲引擎**按运行模式分桶**（speed / balanced / quality）：跨模式复用会把上一模式常驻的
//! 模型带进来，既浪费内存又语义不清；分桶后只复用同模式引擎，总容量仍为
//! `max_concurrent_tasks`（不按模式数翻倍）。容量已满且本模式无空闲引擎时，驱逐其他模式的
//! 空闲引擎腾出容量位（宁可重建也不跨模式复用），避免各模式互相饿死。

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

use photos_core::inference::InferenceEngine;

/// 引擎构造器：按输入图尺寸构造引擎（真实引擎忽略尺寸；演示引擎按尺寸回放）
pub type EngineBuilder = Arc<dyn Fn(u32, u32) -> Box<dyn InferenceEngine> + Send + Sync>;

/// 池内状态（各模式空闲引擎桶 + 已建引擎数，后者为全池总容量口径）
#[derive(Default)]
struct PoolState {
    idle: HashMap<String, Vec<Box<dyn InferenceEngine>>>,
    created: usize,
}

/// 进程级引擎池
pub struct EnginePool {
    builder: EngineBuilder,
    /// 同时存活的引擎上限（至少 1）
    capacity: usize,
    state: Mutex<PoolState>,
    idle_ready: Condvar,
}

/// 借出的引擎：`Drop` 时归还引擎池
pub struct EngineLease {
    /// 归还目标（None = 一次性引擎，用完丢弃）
    pool: Option<Arc<EnginePool>>,
    /// 借出时的运行模式（归还回同模式桶）
    mode: String,
    /// 借出的引擎（None 表示已归还）
    engine: Option<Box<dyn InferenceEngine>>,
    /// 引擎已损坏：归还时直接丢弃（不污染池）并释放一个容量位
    broken: bool,
}

impl EngineLease {
    /// 一次性引擎（演示/测试等按尺寸回放的引擎不入池）
    pub fn owned(engine: Box<dyn InferenceEngine>) -> Self {
        Self {
            pool: None,
            mode: String::new(),
            engine: Some(engine),
            broken: false,
        }
    }

    /// 可变借用引擎（流水线需要 `&mut dyn InferenceEngine`）
    pub fn engine_mut(&mut self) -> &mut dyn InferenceEngine {
        self.engine.as_mut().expect("引擎借用已释放").as_mut()
    }

    /// 标记引擎已损坏：归还时丢弃该引擎并释放容量位，供后续任务新建健康引擎。
    ///
    /// 仅在推理层失败（会话损坏、装载异常等）时调用；业务性失败（未检出人脸、
    /// 读图失败等）不影响引擎健康，不应调用。
    pub fn mark_broken(&mut self) {
        self.broken = true;
    }
}

impl Drop for EngineLease {
    fn drop(&mut self) {
        if let (Some(pool), Some(engine)) = (self.pool.take(), self.engine.take()) {
            if self.broken {
                pool.discard();
            } else {
                pool.put_back(&self.mode, engine);
            }
        }
    }
}

impl EnginePool {
    /// 新建引擎池（容量为同时存活的引擎上限，至少 1）
    pub fn new(builder: EngineBuilder, capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            builder,
            capacity: capacity.max(1),
            state: Mutex::new(PoolState::default()),
            idle_ready: Condvar::new(),
        })
    }

    /// 借出引擎（限 `mode` 模式）：优先复用同模式空闲引擎；未达总容量上限时新建；
    /// 已达上限且本模式无空闲时驱逐其他模式的空闲引擎；否则等待其他任务归还
    pub fn acquire(self: &Arc<Self>, mode: &str, w: u32, h: u32) -> EngineLease {
        let engine = self.take(mode, w, h);
        EngineLease {
            pool: Some(self.clone()),
            mode: mode.to_string(),
            engine: Some(engine),
            broken: false,
        }
    }

    /// 已建引擎数（测试与可观测性用）
    pub fn created(&self) -> usize {
        self.state.lock().unwrap().created
    }

    fn take(&self, mode: &str, w: u32, h: u32) -> Box<dyn InferenceEngine> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(engine) = state.idle.get_mut(mode).and_then(|bucket| bucket.pop()) {
                return engine;
            }
            if state.created < self.capacity {
                state.created += 1;
                // 建模可能耗时较长（装载 ONNX 会话），不持锁构造
                drop(state);
                return (self.builder)(w, h);
            }
            // 容量已满且本模式无空闲：驱逐其他模式的一个空闲引擎，其容量位由本次新建接管
            // （不调整 created，避免并发下短暂超容），避免各模式互相饿死
            let victim = state
                .idle
                .iter_mut()
                .find(|(k, bucket)| k.as_str() != mode && !bucket.is_empty())
                .and_then(|(_, bucket)| bucket.pop());
            if let Some(victim) = victim {
                drop(state);
                drop(victim); // 释放旧会话（可能较慢），不持锁
                return (self.builder)(w, h);
            }
            state = self.idle_ready.wait(state).unwrap();
        }
    }

    fn put_back(&self, mode: &str, engine: Box<dyn InferenceEngine>) {
        if let Ok(mut state) = self.state.lock() {
            state.idle.entry(mode.to_string()).or_default().push(engine);
            self.idle_ready.notify_one();
        }
    }

    /// 丢弃一个已损坏引擎：不归还任何会话，释放一个容量位（由 Drop for EngineLease 调用）
    fn discard(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.created = state.created.saturating_sub(1);
            self.idle_ready.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use photos_core::config::{Config, ExecutionProvider};
    use photos_core::error::CoreResult;
    use photos_core::inference::TensorData;

    use super::*;

    /// 计数引擎：仅用于观测构造次数
    struct CountingEngine;

    impl InferenceEngine for CountingEngine {
        fn load(
            &mut self,
            _cfg: &Config,
            _model_id: &str,
            _provider: ExecutionProvider,
        ) -> CoreResult<()> {
            Ok(())
        }

        fn run(&self, _model_id: &str, _input: &TensorData) -> CoreResult<Vec<TensorData>> {
            Ok(Vec::new())
        }
    }

    /// 计数池：返回池与构造次数计数器
    fn counter_pool(capacity: usize) -> (Arc<EnginePool>, Arc<AtomicUsize>) {
        let built = Arc::new(AtomicUsize::new(0));
        let counter = built.clone();
        let pool = EnginePool::new(
            Arc::new(move |_, _| {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::new(CountingEngine) as Box<dyn InferenceEngine>
            }),
            capacity,
        );
        (pool, built)
    }

    #[test]
    fn 归还后复用同一引擎() {
        let (pool, built) = counter_pool(1);
        {
            let mut lease = pool.acquire("balanced", 100, 100);
            let tensor = TensorData::new(vec![1], vec![0.0]).unwrap();
            assert!(lease.engine_mut().run("任意", &tensor).unwrap().is_empty());
        }
        let _lease = pool.acquire("balanced", 100, 100);
        assert_eq!(
            built.load(Ordering::SeqCst),
            1,
            "第二次借用应复用池中引擎，不得重复构造"
        );
        assert_eq!(pool.created(), 1);
    }

    #[test]
    fn 借出至容量上限后归还再复用() {
        let (pool, built) = counter_pool(2);
        let first = pool.acquire("balanced", 10, 10);
        let second = pool.acquire("balanced", 10, 10);
        assert_eq!(
            built.load(Ordering::SeqCst),
            2,
            "两个并发借出应各建一个引擎"
        );
        drop(first);
        let third = pool.acquire("balanced", 10, 10);
        assert_eq!(
            built.load(Ordering::SeqCst),
            2,
            "归还后应复用空闲引擎，不得新建第三个"
        );
        drop((second, third));
        assert_eq!(pool.created(), 2);
        assert_eq!(built.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn 不同模式不互相复用() {
        let (pool, built) = counter_pool(2);
        {
            let _speed = pool.acquire("speed", 10, 10);
            let _balanced = pool.acquire("balanced", 10, 10);
        }
        assert_eq!(built.load(Ordering::SeqCst), 2, "两个模式应各建一个引擎");
        // 归还后各自复用同模式引擎，不跨模式复用（speed 的引擎不得被 balanced 借走）
        let _speed = pool.acquire("speed", 10, 10);
        let _balanced = pool.acquire("balanced", 10, 10);
        assert_eq!(
            built.load(Ordering::SeqCst),
            2,
            "同模式归还后应复用，不得因跨模式而新建"
        );
        assert_eq!(pool.created(), 2);
    }

    #[test]
    fn 容量满时跨模式借用驱逐空闲引擎而不阻塞() {
        let (pool, built) = counter_pool(1);
        drop(pool.acquire("speed", 10, 10));
        // 容量已满且 balanced 桶为空：应驱逐 speed 的空闲引擎并新建（而非永久等待）
        let _balanced = pool.acquire("balanced", 10, 10);
        assert_eq!(
            built.load(Ordering::SeqCst),
            2,
            "应驱逐异模式空闲引擎并新建本模式引擎"
        );
        assert_eq!(pool.created(), 1, "驱逐后容量位由新引擎接管，总数不变");
    }

    #[test]
    fn 一次性引擎不入池() {
        let mut lease = EngineLease::owned(Box::new(CountingEngine));
        let tensor = TensorData::new(vec![1], vec![0.0]).unwrap();
        assert!(lease.engine_mut().run("任意", &tensor).unwrap().is_empty());
    }

    #[test]
    fn 标记损坏的引擎被驱逐且释放容量位() {
        let (pool, built) = counter_pool(1);
        {
            let mut lease = pool.acquire("balanced", 10, 10);
            lease.mark_broken();
        }
        assert_eq!(pool.created(), 0, "损坏引擎应被丢弃并释放容量位");
        // 容量位释放后应能新建健康引擎（而不是复用损坏的那个）
        let _lease = pool.acquire("balanced", 10, 10);
        assert_eq!(
            built.load(Ordering::SeqCst),
            2,
            "应新建引擎而非复用损坏引擎"
        );
    }
}
