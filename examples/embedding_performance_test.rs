//! Embedding Performance Test
//! 
//! This test measures embedding generation performance and checks for potential
//! blocking behavior that could impact UI responsiveness.

use std::time::{Duration, Instant};
use tokio::time::sleep;
use u_forge_ai::embeddings::EmbeddingManager;
use std::sync::Arc;
use tokio::task;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 Testing embedding performance and blocking behavior...\n");
    
    // Initialize embedding manager
    let cache_dir = std::env::temp_dir().join("embedding_perf_test");
    std::fs::create_dir_all(&cache_dir)?;
    
    println!("📦 Initializing FastEmbed model...");
    let start = Instant::now();
    let embedding_manager = Arc::new(
        EmbeddingManager::try_new_local_default(Some(cache_dir))?
    );
    let provider = embedding_manager.get_provider();
    println!("✅ Model initialized in {:?}\n", start.elapsed());
    
    // Test 1: Single embedding performance
    println!("🔍 Test 1: Single embedding performance");
    let test_text = "This is a test document about Middle-earth and the adventures of Frodo Baggins.";
    
    let start = Instant::now();
    let embedding = provider.embed(test_text).await?;
    let single_duration = start.elapsed();
    
    println!("   Text: '{}...'", &test_text[..50]);
    println!("   Embedding dimensions: {}", embedding.len());
    println!("   Time taken: {:?}", single_duration);
    
    // Test 2: Batch embedding performance
    println!("\n📚 Test 2: Batch embedding performance");
    let batch_texts = vec![
        "Gandalf is a wise wizard from Middle-earth.".to_string(),
        "The Shire is a peaceful region inhabited by hobbits.".to_string(),
        "The One Ring holds great power and corruption.".to_string(),
        "Aragorn is the rightful king of Gondor.".to_string(),
        "Mount Doom is where the Ring was forged and must be destroyed.".to_string(),
    ];
    
    let start = Instant::now();
    let batch_embeddings = provider.embed_batch(batch_texts.clone()).await?;
    let batch_duration = start.elapsed();
    
    println!("   Batch size: {} documents", batch_texts.len());
    println!("   Total embeddings: {}", batch_embeddings.len());
    println!("   Time taken: {:?}", batch_duration);
    println!("   Average per document: {:?}", batch_duration / batch_texts.len() as u32);
    
    // Test 3: Concurrency and blocking behavior
    println!("\n⚡ Test 3: Concurrency and blocking behavior");
    
    // Create a task that should run concurrently while embeddings are being generated
    let counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counter_clone = counter.clone();
    
    // Background task that increments a counter every 1ms
    let background_task = task::spawn(async move {
        for _ in 0..1000 {
            counter_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            sleep(Duration::from_millis(1)).await;
        }
    });
    
    // Embedding task running concurrently
    let provider_clone = provider.clone();
    let embedding_task = task::spawn(async move {
        let start = Instant::now();
        
        // Generate multiple embeddings sequentially
        for i in 0..10 {
            let text = format!("This is test document number {} about various TTRPG elements and worldbuilding concepts.", i);
            let _embedding = provider_clone.embed(&text).await.unwrap();
        }
        
        start.elapsed()
    });
    
    // Wait for both tasks to complete
    let embedding_time = embedding_task.await?;
    let _ = background_task.await?;
    
    let final_count = counter.load(std::sync::atomic::Ordering::Relaxed);
    
    println!("   Embedding task duration: {:?}", embedding_time);
    println!("   Background task counter: {}/1000", final_count);
    
    if final_count < 500 {
        println!("   ⚠️  WARNING: Low counter suggests potential blocking behavior!");
        println!("      Expected ~1000 increments, got {}.", final_count);
        println!("      This could cause UI freezes in a desktop application.");
    } else {
        println!("   ✅ Good concurrency - background task ran smoothly");
    }
    
    // Test 4: CPU-intensive embedding generation with yielding
    println!("\n🔄 Test 4: Testing with manual yielding");
    
    let provider_yield = provider.clone();
    let start = Instant::now();
    
    for i in 0..5 {
        let text = format!("Large document {} with more content to process for embedding generation.", i);
        let _embedding = provider_yield.embed(&text).await?;
        
        // Manually yield to allow other tasks to run
        tokio::task::yield_now().await;
    }
    
    let yield_duration = start.elapsed();
    println!("   Time with yielding: {:?}", yield_duration);
    
    // Test 5: Spawn blocking test
    println!("\n🚧 Test 5: Testing spawn_blocking approach");
    
    let provider_blocking = provider.clone();
    let start = Instant::now();
    
    for i in 0..5 {
        let text = format!("Document {} for spawn_blocking test.", i);
        let provider_clone = provider_blocking.clone();
        
        let _embedding = task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(async {
                provider_clone.embed(&text).await
            })
        }).await??;
    }
    
    let blocking_duration = start.elapsed();
    println!("   Time with spawn_blocking: {:?}", blocking_duration);
    
    // Summary and recommendations
    println!("\n📊 Performance Summary:");
    println!("   Single embedding: {:?}", single_duration);
    println!("   Batch embedding: {:?} ({:?}/doc)", batch_duration, batch_duration / batch_texts.len() as u32);
    println!("   Sequential embeddings: {:?}", embedding_time);
    println!("   With yielding: {:?}", yield_duration);
    println!("   With spawn_blocking: {:?}", blocking_duration);
    
    println!("\n💡 Recommendations for UI integration:");
    
    if single_duration > Duration::from_millis(100) {
        println!("   ⚠️  Single embeddings take >{:?} - consider background processing", Duration::from_millis(100));
    }
    
    if final_count < 800 {
        println!("   🔧 REQUIRED: Implement spawn_blocking for embedding operations");
        println!("      Current implementation may block UI thread");
    } else {
        println!("   ✅ Current async implementation appears non-blocking");
    }
    
    if batch_duration > Duration::from_millis(500) {
        println!("   📊 Batch operations >{:?} - use progress indicators", Duration::from_millis(500));
    }
    
    println!("\n🎯 For Tauri integration:");
    println!("   • Use spawn_blocking for embedding operations");
    println!("   • Show progress indicators for batch processing");
    println!("   • Consider embedding queue with background worker");
    println!("   • Cache embeddings to avoid regeneration");
    
    Ok(())
}