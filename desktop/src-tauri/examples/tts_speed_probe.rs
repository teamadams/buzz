use std::time::Instant;

const SAMPLE_RATE: u32 = 24_000;

fn main() {
    let input: Vec<f32> = (0..SAMPLE_RATE * 10)
        .map(|index| {
            (2.0 * std::f32::consts::PI * 220.0 * index as f32 / SAMPLE_RATE as f32).sin() * 0.25
        })
        .collect();

    for speed in [0.75_f32, 1.25, 1.5] {
        let mut processor = ssstretch::Stretch::new();
        processor.preset_default(1, SAMPLE_RATE as f32);
        let output_len = (input.len() as f32 / speed).round() as usize;
        let inputs = [input.clone()];
        let mut outputs = [Vec::with_capacity(output_len)];
        let latency_ms = processor.output_latency() as f64 * 1_000.0 / SAMPLE_RATE as f64;
        let started = Instant::now();
        processor.process_vec(&inputs, input.len() as i32, &mut outputs, output_len as i32);
        let elapsed = started.elapsed();
        println!(
            "{speed:.2}x: latency={latency_ms:.1}ms, CPU={:.2}ms for 10s ({:.3}% realtime), output={}",
            elapsed.as_secs_f64() * 1_000.0,
            elapsed.as_secs_f64() / 10.0 * 100.0,
            outputs[0].len(),
        );
    }
}
