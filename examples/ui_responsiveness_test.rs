//! UI Responsiveness Test
//! 
//! This test simulates a desktop UI workload while embedding operations are running
//! to ensure that embedding generation doesn't interfere with UI responsiveness.

use std::time::{Duration, Instant};
use tokio::time::{sleep, interval};
use u_forge_ai::embeddings::EmbeddingManager;
use std::sync::Arc;
use tokio::task;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎮 Testing UI responsiveness during embedding operations...\n");
    
    // Initialize embedding manager
    let cache_dir = std::env::temp_dir().join("ui_responsiveness_test");
    std::fs::create_dir_all(&cache_dir)?;
    
    println!("📦 Initializing FastEmbed model...");
    let start = Instant::now();
    let embedding_manager = Arc::new(
        EmbeddingManager::try_new_local_default(Some(cache_dir))?
    );
    let provider = embedding_manager.get_provider();
    println!("✅ Model initialized in {:?}\n", start.elapsed());
    
    // Test 1: High-frequency UI simulation
    println!("🎮 Test 1: High-frequency UI updates during embedding");
    
    let frame_counter = Arc::new(AtomicU64::new(0));
    let stop_flag = Arc::new(AtomicBool::new(false));
    
    // Simulate 60 FPS UI updates
    let frame_counter_clone = frame_counter.clone();
    let stop_flag_clone = stop_flag.clone();
    let ui_task = task::spawn(async move {
        let mut interval = interval(Duration::from_millis(16)); // ~60 FPS
        while !stop_flag_clone.load(Ordering::Relaxed) {
            interval.tick().await;
            frame_counter_clone.fetch_add(1, Ordering::Relaxed);
            
            // Simulate some UI work
            let _work = (0..1000).map(|i| i * i).collect::<Vec<_>>();
        }
    });
    
    // Run embedding operations concurrently
    let provider_clone = provider.clone();
    let embedding_task = task::spawn(async move {
        let start = Instant::now();
        
        // Generate embeddings for various TTRPG content
        let ttrpg_texts = vec![
            "A mysterious wizard appears at the tavern, his robes shimmering with arcane energy.",
            "The ancient dragon's lair is filled with treasures beyond imagination, but also deadly traps.",
            "In the shadowy alleyways of the capital city, thieves and rogues conduct their business.",
            "The paladin raises his holy symbol, channeling divine power to heal his wounded companions.",
            "Deep in the dungeon, strange runes glow with an otherworldly light, warning of ancient curses.",
            "The ranger tracks the orc warband through the dense forest, following broken branches and footprints.",
            "At the royal court, nobles scheme and plot while maintaining facades of friendship and loyalty.",
            "The barbarian's rage knows no bounds as he charges into battle, his axe gleaming in the moonlight.",
            "In the wizard's tower, countless tomes contain secrets of magic that could reshape the world.",
            "The bard's tale captivates the audience, weaving magic through words and melody.",
        ];
        
        for (i, text) in ttrpg_texts.iter().enumerate() {
            println!("   Embedding document {}/{}...", i + 1, ttrpg_texts.len());
            let _embedding = provider_clone.embed(text).await.unwrap();
            
            // Small delay to simulate real-world usage
            sleep(Duration::from_millis(50)).await;
        }
        
        start.elapsed()
    });
    
    // Let it run for the duration of the embedding task
    let embedding_duration = embedding_task.await?;
    stop_flag.store(true, Ordering::Relaxed);
    ui_task.await?;
    
    let total_frames = frame_counter.load(Ordering::Relaxed);
    let expected_frames = embedding_duration.as_millis() / 16; // 60 FPS
    let frame_rate = (total_frames as f64 / embedding_duration.as_secs_f64()) as u64;
    
    println!("   Embedding duration: {:?}", embedding_duration);
    println!("   Total UI frames: {}", total_frames);
    println!("   Expected frames (~60 FPS): {}", expected_frames);
    println!("   Actual frame rate: {} FPS", frame_rate);
    
    if frame_rate < 45 {
        println!("   ⚠️  WARNING: Frame rate dropped below 45 FPS - potential UI lag!");
    } else if frame_rate < 55 {
        println!("   ⚡ CAUTION: Frame rate slightly reduced - consider optimization");
    } else {
        println!("   ✅ Excellent frame rate maintained during embedding operations");
    }
    
    // Test 2: CPU-intensive background processing
    println!("\n🔧 Test 2: CPU-intensive tasks with spawn_blocking");
    
    let cpu_counter = Arc::new(AtomicU64::new(0));
    let stop_cpu = Arc::new(AtomicBool::new(false));
    
    // CPU-intensive background task
    let cpu_counter_clone = cpu_counter.clone();
    let stop_cpu_clone = stop_cpu.clone();
    let cpu_task = task::spawn(async move {
        while !stop_cpu_clone.load(Ordering::Relaxed) {
            // Simulate CPU work
            let _work: u64 = (0..10000).map(|i| i as u64 * i as u64).sum();
            cpu_counter_clone.fetch_add(1, Ordering::Relaxed);
            task::yield_now().await;
        }
    });
    
    // Test embedding with spawn_blocking
    let provider_blocking = provider.clone();
    let start = Instant::now();
    
    for i in 0..5 {
        let text = format!("Complex TTRPG document {} with detailed world-building content that requires embedding processing.", i);
        let provider_clone = provider_blocking.clone();
        
        let _embedding = task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async {
                provider_clone.embed(&text).await
            })
        }).await??;
        
        println!("   Processed blocking embedding {}/5", i + 1);
    }
    
    let blocking_duration = start.elapsed();
    stop_cpu.store(true, Ordering::Relaxed);
    cpu_task.await?;
    
    let cpu_iterations = cpu_counter.load(Ordering::Relaxed);
    println!("   Blocking embedding duration: {:?}", blocking_duration);
    println!("   CPU task iterations: {}", cpu_iterations);
    
    if cpu_iterations < 1000 {
        println!("   ⚠️  WARNING: Low CPU task iterations - spawn_blocking may be too aggressive");
    } else {
        println!("   ✅ Good CPU task performance with spawn_blocking");
    }
    
    // Test 3: Embedding queue simulation
    println!("\n📋 Test 3: Embedding queue with background worker");
    
    // Create a queue simulation
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(32);
    let queue_provider = provider.clone();
    
    // Background worker
    let worker_task = task::spawn(async move {
        let mut processed = 0;
        while let Some(text) = rx.recv().await {
            let start = Instant::now();
            
            // Use spawn_blocking for the embedding operation
            let provider_clone = queue_provider.clone();
            let _embedding = task::spawn_blocking(move || {
                tokio::runtime::Handle::current().block_on(async {
                    provider_clone.embed(&text).await
                })
            }).await.unwrap().unwrap();
            
            processed += 1;
            println!("   Queue worker processed item {} in {:?}", processed, start.elapsed());
        }
        processed
    });
    
    // Simulate rapid embedding requests
    let queue_texts = vec![
        "Character backstory for a half-elf ranger",
        "Location description of a haunted castle",
        "Plot hook involving political intrigue",
        "Combat encounter with undead creatures",
        "Merchant's inventory and shop description",
    ];
    
    println!("   Queuing {} embedding requests...", queue_texts.len());
    for text in queue_texts {
        tx.send(text.to_string()).await?;
    }
    drop(tx); // Close the channel
    
    let processed_count = worker_task.await?;
    println!("   ✅ Queue processed {} items successfully", processed_count);
    
    // Summary and recommendations
    println!("\n📊 UI Responsiveness Summary:");
    println!("   Frame rate during embeddings: {} FPS", frame_rate);
    println!("   Blocking operation duration: {:?}", blocking_duration);
    println!("   Queue processing: {} items", processed_count);
    
    println!("\n💡 Recommendations for Tauri integration:");
    
    if frame_rate >= 55 {
        println!("   ✅ Current implementation suitable for UI use");
    } else {
        println!("   🔧 IMPLEMENT: spawn_blocking for all embedding operations");
    }
    
    println!("   📋 RECOMMENDED: Implement embedding queue system");
    println!("   ⚡ RECOMMENDED: Show progress indicators for long operations");
    println!("   💾 RECOMMENDED: Cache embeddings to avoid recomputation");
    println!("   🎯 RECOMMENDED: Batch multiple requests when possible");
    
    println!("\n🛠️  Next steps for implementation:");
    println!("   1. Wrap all embedding calls in spawn_blocking");
    println!("   2. Create EmbeddingQueue with background worker");
    println!("   3. Add progress callbacks for UI updates");
    println!("   4. Implement embedding cache with persistence");
    println!("   5. Add cancellation support for long-running operations");
    
    Ok(())
}