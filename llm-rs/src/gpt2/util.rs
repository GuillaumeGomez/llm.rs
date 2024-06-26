use std::f32::consts::PI;
use std::sync::atomic::{AtomicPtr, Ordering};
use rayon::prelude::*;

// ----------------------------------------------------------------------------
// All the individual layers' forward and backward passes
// B = batch_size, T = sequence_length, C = channels, V = vocab_size
// ----------------------------------------------------------------------------

/// Computes the forward pass for the encoder, combining token and positional embeddings.
///
/// # Arguments
///
/// * `out` - Output tensor for combined embeddings.
/// * `inp` - Input tensor containing token indices.
/// * `wte` - Token embedding matrix.
/// * `wpe` - Positional embedding matrix.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Embedding dimension.
pub unsafe fn encoder_forward(
    out: *mut f32,
    inp: *const i32,
    wte: *const f32,
    wpe: *const f32,
    B: usize,
    T: usize,
    C: usize,
) {
    for b in 0..B {
        for t in 0..T {
            // Calculate the base address for out[b,t,:]
            let out_bt = out.add(b * T * C + t * C);
            // Get the token index at inp[b, t]
            let ix = *inp.add(b * T + t) as usize;
            // Calculate the base address for wte[ix,:]
            let wte_ix = wte.add(ix * C);
            // Calculate the base address for wpe[t,:]
            let wpe_t = wpe.add(t * C);
            // Sum the token and position embeddings and store the result in out[b,t,:]
            for i in 0..C {
                *out_bt.add(i) = *wte_ix.add(i) + *wpe_t.add(i);
            }
        }
    }
}

/// Computes the backward pass for the encoder, updating gradients for token and position embeddings.
///
/// # Arguments
///
/// * `dwte` - Gradient of the token embedding matrix.
/// * `dwpe` - Gradient of the positional embedding matrix.
/// * `dout` - Gradient of the output tensor.
/// * `inp` - Input tensor containing token indices.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Embedding dimension.
pub unsafe fn encoder_backward(
    dwte: *mut f32,
    dwpe: *mut f32,
    dout: *const f32,
    inp: *const i32,
    B: usize,
    T: usize,
    C: usize,
) {
    for b in 0..B {
        for t in 0..T {
            // Calculate the base address for dout[b,t,:]
            let dout_bt = dout.add(b * T * C + t * C);
            // Get the token index at inp[b, t]
            let ix = *inp.add(b * T + t) as usize;
            // Calculate the base address for dwte[ix,:]
            let dwte_ix = dwte.add(ix * C);
            // Calculate the base address for dwpe[t,:]
            let dwpe_t = dwpe.add(t * C);
            // Accumulate the gradients from dout into dwte and dwpe
            for i in 0..C {
                let d = *dout_bt.add(i);
                *dwte_ix.add(i) += d;
                *dwpe_t.add(i) += d;
            }
        }
    }
}

/// Computes the forward pass for Layer Normalization, producing normalized output,
/// and caching mean and reciprocal standard deviation.
///
/// # Arguments
///
/// * `out` - Output tensor for the normalized result.
/// * `mean` - Buffer to store the mean values.
/// * `rstd` - Buffer to store the reciprocal of the standard deviation.
/// * `inp` - Input tensor.
/// * `weight` - Weight vector for scaling.
/// * `bias` - Bias vector for shifting.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Feature dimension.
/// 
/// # Note
/// 
/// Reference: https://pytorch.org/docs/stable/generated/torch.nn.LayerNorm.html
pub unsafe fn layernorm_forward(
    out: *mut f32,
    mean: *mut f32,
    rstd: *mut f32,
    inp: *const f32,
    weight: *const f32,
    bias: *const f32,
    B: usize,
    T: usize,
    C: usize,
) {
    let eps: f32 = 1e-5;

    for b in 0..B {
        for t in 0..T {
            // Calculate the base address for inp[b,t,:]
            let x = inp.add(b * T * C + t * C);

            // Calculate the mean
            let mut m: f32 = 0.0;
            for i in 0..C {
                m += *x.add(i);
            }
            m /= C as f32;

            // Calculate the variance
            let mut v: f32 = 0.0;
            for i in 0..C {
                let xshift = *x.add(i) - m;
                v += xshift * xshift;
            }
            v /= C as f32;

            // Calculate the rstd (reciprocal standard deviation)
            let s: f32 = 1.0 / (v + eps).sqrt();

            // Calculate the base address for out[b,t,:]
            let out_bt = out.add(b * T * C + t * C);
            for i in 0..C {
                let n = s * (*x.add(i) - m); // Normalize
                let o = n * *weight.add(i) + *bias.add(i); // Scale and shift
                *out_bt.add(i) = o; // Write
            }

            // Cache the mean and rstd for the backward pass
            *mean.add(b * T + t) = m;
            *rstd.add(b * T + t) = s;
        }
    }
}

/// Computes the backward pass for Layer Normalization, updating gradients for inputs,
/// weights, and biases.
///
/// # Arguments
///
/// * `dinp` - Gradient of the input tensor.
/// * `dweight` - Gradient of the weight vector.
/// * `dbias` - Gradient of the bias vector.
/// * `dout` - Gradient of the output tensor.
/// * `inp` - Input tensor.
/// * `weight` - Weight vector.
/// * `mean` - Mean of the input tensor across the normalization axis.
/// * `rstd` - Reciprocal of the standard deviation of the input tensor.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Feature dimension.
pub unsafe fn layernorm_backward(
    dinp: *mut f32,
    dweight: *mut f32,
    dbias: *mut f32,
    dout: *const f32,
    inp: *const f32,
    weight: *const f32,
    mean: *const f32,
    rstd: *const f32,
    B: usize,
    T: usize,
    C: usize,
) {
    for b in 0..B {
        for t in 0..T {
            // Calculate the base addresses
            let dout_bt = dout.add(b * T * C + t * C);
            let inp_bt = inp.add(b * T * C + t * C);
            let dinp_bt = dinp.add(b * T * C + t * C);
            let mean_bt = *mean.add(b * T + t);
            let rstd_bt = *rstd.add(b * T + t);

            // First: two reduce operations
            let mut dnorm_mean: f32 = 0.0;
            let mut dnorm_norm_mean: f32 = 0.0;
            for i in 0..C {
                let norm_bti = (*inp_bt.add(i) - mean_bt) * rstd_bt;
                let dnorm_i = *weight.add(i) * *dout_bt.add(i);
                dnorm_mean += dnorm_i;
                dnorm_norm_mean += dnorm_i * norm_bti;
            }
            dnorm_mean /= C as f32;
            dnorm_norm_mean /= C as f32;

            // Now iterate again and accumulate all the gradients
            for i in 0..C {
                let norm_bti = (*inp_bt.add(i) - mean_bt) * rstd_bt;
                let dnorm_i = *weight.add(i) * *dout_bt.add(i);

                // Gradient contribution to bias
                *dbias.add(i) += *dout_bt.add(i);

                // Gradient contribution to weight
                *dweight.add(i) += norm_bti * *dout_bt.add(i);

                // Gradient contribution to input
                let mut dval: f32 = 0.0;
                dval += dnorm_i; // Term 1
                dval -= dnorm_mean; // Term 2
                dval -= norm_bti * dnorm_norm_mean; // Term 3
                dval *= rstd_bt; // Final scale
                *dinp_bt.add(i) += dval;
            }
        }
    }
}

/// Naive implementation of the forward pass for matrix multiplication, producing the output tensor.
///
/// # Arguments
///
/// * `out` - Output tensor for the matrix multiplication result.
/// * `inp` - Input tensor.
/// * `weight` - Weight matrix.
/// * `bias` - Bias vector.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Input feature dimension.
/// * `OC` - Output feature dimension or output channels.
/// 
/// # Note
/// 
/// This is the most naive implementation of matrix multiplication that serves as an algorithmic reference, and as a fallback for unfriendly input shapes inside matmul_forward().
pub unsafe fn matmul_forward_naive(
    out: *mut f32,
    inp: *const f32,
    weight: *const f32,
    bias: *const f32,
    B: usize,
    T: usize,
    C: usize,
    OC: usize,
) {
    let out_atomic = AtomicPtr::new(out);
    let inp_atomic = AtomicPtr::new(inp as *mut f32);
    let weight_atomic = AtomicPtr::new(weight as *mut f32);
    let bias_atomic = AtomicPtr::new(bias as *mut f32);

    // Create a parallel iterator over the batch dimension
    (0..B).into_par_iter().for_each(|b| {
        // Create a parallel iterator over the sequence length
        (0..T).into_par_iter().for_each(|t| {
            // Load the AtomicPtr values into raw pointers for the current scope
            let out_raw = out_atomic.load(Ordering::SeqCst);
            let inp_raw = inp_atomic.load(Ordering::SeqCst);
            let weight_raw = weight_atomic.load(Ordering::SeqCst);
            let bias_raw = bias_atomic.load(Ordering::SeqCst);

            let bt = b * T + t;
            // Iterate over the output channels
            for o in 0..OC {
                // Initialize the output value with the bias if provided, otherwise 0.0
                let mut val = if !bias_raw.is_null() {
                    *bias_raw.add(o)
                } else {
                    0.0f32
                };
                // Perform the dot product
                for i in 0..C {
                    val += *inp_raw.add(bt * C + i) * *weight_raw.add(o * C + i);
                }
                // Store the result
                *out_raw.add(bt * OC + o) = val;
            }
        });
    });
}

/// Computes the forward pass for matrix multiplication, producing the output tensor.
///
/// # Arguments
///
/// * `out` - Output tensor for the matrix multiplication result.
/// * `inp` - Input tensor.
/// * `weight` - Weight matrix.
/// * `bias` - Bias vector.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Input feature dimension.
/// * `OC` - Output feature dimension or output channels.
/// 
/// # Note
/// 
/// Most of the running time is spent here and in matmul_backward, therefore, the implementation below is very mildly optimized.
/// This function is otherwise identical to that of matmul_forward_naive().
pub unsafe fn matmul_forward(
    out: *mut f32,
    inp: *const f32,
    weight: *const f32,
    bias: *const f32,
    B: usize,
    T: usize,
    C: usize,
    OC: usize,
) {
    const LOOP_UNROLL: usize = 8;
    let out_atomic = AtomicPtr::new(out);
    let inp_atomic = AtomicPtr::new(inp as *mut f32);
    let weight_atomic = AtomicPtr::new(weight as *mut f32);
    let bias_atomic = AtomicPtr::new(bias as *mut f32);

    // Fallback to naive implementation if B * T is not a multiple of LOOP_UNROLL
    if (B * T) % LOOP_UNROLL != 0 {
        matmul_forward_naive(out, inp, weight, bias, B, T, C, OC);
        return;
    }

    // Parallelize the outer loop using Rayon
    (0..B * T).into_par_iter().step_by(LOOP_UNROLL).for_each(|obt| {
        // Load the AtomicPtr values into raw pointers for the current scope
        let out_raw = out_atomic.load(Ordering::SeqCst);
        let inp_raw = inp_atomic.load(Ordering::SeqCst);
        let weight_raw = weight_atomic.load(Ordering::SeqCst);
        let bias_raw = bias_atomic.load(Ordering::SeqCst);

        for o in 0..OC {
            // Initialize the result array with bias if present
            let mut result = [0.0f32; LOOP_UNROLL];
            for ibt in 0..LOOP_UNROLL {
                result[ibt] = if !bias_raw.is_null() { *bias_raw.add(o) } else { 0.0f32 };
            }

            // Cache the weight value and compute dot products
            for i in 0..C {
                let w = *weight_raw.add(i + o * C);
                for ibt in 0..LOOP_UNROLL {
                    let bt = obt + ibt;
                    result[ibt] += *inp_raw.add(bt * C + i) * w;
                }
            }

            // Write results back to the output matrix
            for ibt in 0..LOOP_UNROLL {
                let bt = obt + ibt;
                *out_raw.add(bt * OC + o) = result[ibt];
            }
        }
    });
}

/// Computes the backward pass for matrix multiplication, updating gradients for inputs,
/// weights, and biases.
///
/// # Arguments
///
/// * `dinp` - Gradient of the input tensor.
/// * `dweight` - Gradient of the weight matrix.
/// * `dbias` - Gradient of the bias vector.
/// * `dout` - Gradient of the output tensor.
/// * `inp` - Input tensor.
/// * `weight` - Weight matrix.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Input feature dimension.
/// * `OC` - Output feature dimension.
/// 
/// # Note
/// 
/// Most of the running time is spent here and in matmul_forward.
/// This backward could be done in a single "round" of loops but that doesn't afford an efficient parallelization strategy
pub unsafe fn matmul_backward(
    dinp: *mut f32,
    dweight: *mut f32,
    dbias: *mut f32,
    dout: *const f32,
    inp: *const f32,
    weight: *const f32,
    B: usize,
    T: usize,
    C: usize,
    OC: usize,
) {
    let dinp_atomic = AtomicPtr::new(dinp);
    let dweight_atomic = AtomicPtr::new(dweight);
    let dbias_atomic = AtomicPtr::new(dbias);
    let dout_atomic = AtomicPtr::new(dout as *mut f32);
    let inp_atomic = AtomicPtr::new(inp as *mut f32);
    let weight_atomic = AtomicPtr::new(weight as *mut f32);

    // Parallelize over B and T for input gradient computation
    (0..B).into_par_iter().for_each(|b| {
        (0..T).into_par_iter().for_each(|t| {
            // Load the AtomicPtr values into raw pointers for the current scope
            let dout_raw = dout_atomic.load(Ordering::SeqCst);
            let dinp_raw = dinp_atomic.load(Ordering::SeqCst);
            let weight_raw = weight_atomic.load(Ordering::SeqCst);

            // Calculate the base addresses for dout and dinp slices
            let dout_bt = dout_raw.add(b * T * OC + t * OC);
            let dinp_bt = dinp_raw.add(b * T * C + t * C);

            for o in 0..OC {
                let wrow = weight_raw.add(o * C);
                let d = *dout_bt.add(o);
                for i in 0..C {
                    *dinp_bt.add(i) += *wrow.add(i) * d;
                }
            }
        });
    });

    // Parallelize over output channels for weight and bias gradient computation
    (0..OC).into_par_iter().for_each(|o| {
        for b in 0..B {
            for t in 0..T {
                // Load the AtomicPtr values into raw pointers for the current scope
                let dout_raw = dout_atomic.load(Ordering::SeqCst);
                let inp_raw = inp_atomic.load(Ordering::SeqCst);
                let dweight_raw = dweight_atomic.load(Ordering::SeqCst);
                let dbias_raw = dbias_atomic.load(Ordering::SeqCst);

                // Calculate the base addresses for dout and inp slices
                let dout_bt = dout_raw.add(b * T * OC + t * OC);
                let inp_bt = inp_raw.add(b * T * C + t * C);
                let dwrow = dweight_raw.add(o * C);

                let d = *dout_bt.add(o);
                // Update dbias if not null
                if !dbias_raw.is_null() {
                    *dbias_raw.add(o) += d;
                }
                // Update dweight
                for i in 0..C {
                    *dwrow.add(i) += *inp_bt.add(i) * d;
                }
            }
        }
    });
}

/// Computes the forward pass for multi-head attention, generating output and storing attention scores.
///
/// # Arguments
///
/// * `out` - Output tensor for attention results.
/// * `preatt` - Pre-attention scores.
/// * `att` - Post-attention scores.
/// * `inp` - Input tensor containing query, key, and value vectors.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Feature dimension.
/// * `NH` - Number of attention heads.
pub unsafe fn attention_forward(
    out: *mut f32,
    preatt: *mut f32,
    att: *mut f32,
    inp: *const f32,
    B: usize,
    T: usize,
    C: usize,
    NH: usize,
) {
    let C3 = C * 3; // feature dimension scaled by 3
    let hs = C / NH; // head size
    let scale = 1.0 / (hs as f32).sqrt(); // scale for dot product

    let out_atomic = AtomicPtr::new(out);
    let preatt_atomic = AtomicPtr::new(preatt);
    let att_atomic = AtomicPtr::new(att);
    let inp_atomic = AtomicPtr::new(inp as *mut f32);

    (0..B).into_par_iter().for_each(|b| {
        (0..T).into_par_iter().for_each(|t| {
            (0..NH).into_par_iter().for_each(|h| {
                // Load the AtomicPtr values into raw pointers for the current scope
                let out_raw = out_atomic.load(Ordering::SeqCst);
                let preatt_raw = preatt_atomic.load(Ordering::SeqCst);
                let att_raw = att_atomic.load(Ordering::SeqCst);
                let inp_raw = inp_atomic.load(Ordering::SeqCst);

                // Calculate the base addresses
                let query_t = inp_raw.add(b * T * C3 + t * C3 + h * hs);
                let preatt_bth = preatt_raw.add(b * NH * T * T + h * T * T + t * T);
                let att_bth = att_raw.add(b * NH * T * T + h * T * T + t * T);

                // Pass 1: Calculate query dot key and maxval
                let mut maxval = f32::NEG_INFINITY; // Using f32::NEG_INFINITY for better initial value
                for t2 in 0..=t {
                    let key_t2 = inp_raw.add(b * T * C3 + t2 * C3 + h * hs + C); // +C for key
                    let mut val = 0.0;
                    for i in 0..hs {
                        val += *query_t.add(i) * *key_t2.add(i);
                    }
                    val *= scale;
                    if val > maxval {
                        maxval = val;
                    }
                    *preatt_bth.add(t2) = val;
                }

                // Pass 2: Calculate the exp and keep track of sum
                let mut expsum = 0.0;
                for t2 in 0..=t {
                    let expv = (*preatt_bth.add(t2) - maxval).exp();
                    expsum += expv;
                    *att_bth.add(t2) = expv;
                }
                let expsum_inv = if expsum == 0.0 { 0.0 } else { 1.0 / expsum };

                // Pass 3: Normalize to get the softmax
                for t2 in 0..T {
                    if t2 <= t {
                        *att_bth.add(t2) *= expsum_inv;
                    } else {
                        *att_bth.add(t2) = 0.0;
                    }
                }

                // Pass 4: Accumulate weighted values into the output of attention
                let out_bth = out_raw.add(b * T * C + t * C + h * hs);
                for i in 0..hs {
                    *out_bth.add(i) = 0.0;
                }
                for t2 in 0..=t {
                    let value_t2 = inp_raw.add(b * T * C3 + t2 * C3 + h * hs + 2 * C); // +C*2 for value
                    let att_btht2 = *att_bth.add(t2);
                    for i in 0..hs {
                        *out_bth.add(i) += att_btht2 * *value_t2.add(i);
                    }
                }
            });
        });
    });
}

/// Computes the backward pass for attention mechanisms, updating gradients for inputs,
/// pre-attention weights, and attention weights.
///
/// # Arguments
///
/// * `dinp` - Gradient of the input tensor.
/// * `dpreatt` - Gradient of the pre-attention weights.
/// * `datt` - Gradient of the attention weights.
/// * `dout` - Gradient of the output tensor.
/// * `inp` - Input tensor.
/// * `att` - Attention weights.
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `C` - Feature dimension.
/// * `NH` - Number of attention heads.
pub unsafe fn attention_backward(
    dinp: *mut f32,
    dpreatt: *mut f32,
    datt: *mut f32,
    dout: *const f32,
    inp: *const f32,
    att: *const f32,
    B: usize,
    T: usize,
    C: usize,
    NH: usize,
) {
    let C3 = C * 3; // feature dimension scaled by 3
    let hs = C / NH; // head size
    let scale = 1.0 / (hs as f32).sqrt(); // scale for dot product

    for b in 0..B {
        for t in 0..T {
            for h in 0..NH {
                let att_bth = att.add(b * NH * T * T + h * T * T + t * T);
                let datt_bth = datt.add(b * NH * T * T + h * T * T + t * T);
                let dpreatt_bth = dpreatt.add(b * NH * T * T + h * T * T + t * T);
                let dquery_t = dinp.add(b * T * C3 + t * C3 + h * hs);
                let query_t = inp.add(b * T * C3 + t * C3 + h * hs);

                // Backward pass 4: through the value accumulation
                let dout_bth = dout.add(b * T * C + t * C + h * hs);
                for t2 in 0..=t {
                    let value_t2 = inp.add(b * T * C3 + t2 * C3 + h * hs + 2 * C); // +C*2 because it's value
                    let dvalue_t2 = dinp.add(b * T * C3 + t2 * C3 + h * hs + 2 * C); // +C*2 because it's value
                    for i in 0..hs {
                        *datt_bth.add(t2) += *value_t2.add(i) * *dout_bth.add(i);
                        *dvalue_t2.add(i) += *att_bth.add(t2) * *dout_bth.add(i);
                    }
                }

                // Backward pass 2 & 3: the softmax
                for t2 in 0..=t {
                    for t3 in 0..=t {
                        let indicator = if t2 == t3 { 1.0 } else { 0.0 };
                        let local_derivative = *att_bth.add(t2) * (indicator - *att_bth.add(t3));
                        *dpreatt_bth.add(t3) += local_derivative * *datt_bth.add(t2);
                    }
                }

                // Backward pass 1: the query @ key matmul
                for t2 in 0..=t {
                    let key_t2 = inp.add(b * T * C3 + t2 * C3 + h * hs + C); // +C because it's key
                    let dkey_t2 = dinp.add(b * T * C3 + t2 * C3 + h * hs + C); // +C because it's key
                    for i in 0..hs {
                        *dquery_t.add(i) += *key_t2.add(i) * *dpreatt_bth.add(t2) * scale;
                        *dkey_t2.add(i) += *query_t.add(i) * *dpreatt_bth.add(t2) * scale;
                    }
                }
            }
        }
    }
}

/// Applies the GELU activation function to the input tensor.
///
/// # Arguments
///
/// * `out` - Output tensor to store the GELU results.
/// * `inp` - Input tensor.
/// * `N` - Number of elements.
pub unsafe fn gelu_forward(
    out: *mut f32, 
    inp: *const f32, 
    N: usize
) {
    // Process each element
    for i in 0..N {
        // Load the input value
        let x = *inp.add(i);
        // Calculate the cubic term
        let cube = 0.044715 * x * x * x;
        // Apply the GeLU function
        *out.add(i) = 0.5 * x * (1.0 + ((2.0 / PI).sqrt() * (x + cube)).tanh());
    }
}

/// Computes the gradient of the GELU activation function.
///
/// # Arguments
///
/// * `dinp` - Gradient of the input tensor.
/// * `inp` - Input tensor.
/// * `dout` - Gradient of the output tensor.
/// * `N` - Number of elements.
pub unsafe fn gelu_backward(
    dinp: *mut f32,
    inp: *const f32,
    dout: *const f32,
    N: usize,
) {
    let gelu_scaling_factor = (2.0 / PI).sqrt();

    for i in 0..N {
        // Load the input value
        let x = *inp.add(i);
        let dout_val = *dout.add(i);
        
        // Compute the cubic term
        let cube = 0.044715 * x * x * x;
        
        // Compute the argument and the output of the tanh function
        let tanh_arg = gelu_scaling_factor * (x + cube);
        let tanh_out = tanh_arg.tanh();
        
        // Compute the hyperbolic cosine and sech (hyperbolic secant)
        let coshf_out = tanh_arg.cosh();
        let sech_out = 1.0 / (coshf_out * coshf_out);
        
        // Compute the local gradient
        let local_grad = 0.5 * (1.0 + tanh_out) + x * 0.5 * sech_out * gelu_scaling_factor * (1.0 + 3.0 * 0.044715 * x * x);
        
        // Accumulate the gradient into dinp
        *dinp.add(i) += local_grad * dout_val;
    }
}

/// Adds two input tensors element-wise and stores the result in the output tensor.
///
/// # Arguments
///
/// * `out` - Output tensor to store the result.
/// * `inp1` - First input tensor.
/// * `inp2` - Second input tensor.
/// * `N` - Number of elements.
pub unsafe fn residual_forward(
    out: *mut f32,
    inp1: *const f32,
    inp2: *const f32,
    N: usize,
) {
    for i in 0..N {
        *out.add(i) = *inp1.add(i) + *inp2.add(i);
    }
}

/// Accumulates gradients for two input tensors using the gradient of the output tensor.
///
/// # Arguments
///
/// * `dinp1` - Gradient of the first input tensor.
/// * `dinp2` - Gradient of the second input tensor.
/// * `dout` - Gradient of the output tensor.
/// * `N` - Number of elements.
pub unsafe fn residual_backward(
    dinp1: *mut f32,
    dinp2: *mut f32,
    dout: *const f32,
    N: usize,
) {
    for i in 0..N {
        *dinp1.add(i) += *dout.add(i);
        *dinp2.add(i) += *dout.add(i);
    }
}

/// Computes the softmax probabilities from logits in parallel.
///
/// # Arguments
///
/// * `probs` - Output probabilities (B, T, Vp).
/// * `logits` - Input unnormalized log probabilities (B, T, Vp).
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `V` - Real vocabulary size.
/// * `Vp` - Padded vocabulary size.
pub unsafe fn softmax_forward(
    probs: *mut f32,
    logits: *const f32,
    B: usize,
    T: usize,
    V: usize,
    Vp: usize,
) {
    let probs_atomic = AtomicPtr::new(probs);
    let logits_atomic = AtomicPtr::new(logits as *mut f32);

    (0..B).into_par_iter().for_each(|b| {
        (0..T).into_par_iter().for_each(|t| {
            // Load the AtomicPtr values into raw pointers for the current scope
            let probs_raw = probs_atomic.load(Ordering::SeqCst);
            let logits_raw = logits_atomic.load(Ordering::SeqCst);

            // Calculate the base addresses
            let logits_bt = logits_raw.add(b * T * Vp + t * Vp);
            let probs_bt = probs_raw.add(b * T * Vp + t * Vp);

            // Calculate maxval for numerical stability
            let mut maxval = f32::NEG_INFINITY;
            for i in 0..V {
                let logit = *logits_bt.add(i);
                if logit > maxval {
                    maxval = logit;
                }
            }

            // Calculate softmax numerator and denominator (sum)
            let mut sum = 0.0;
            for i in 0..V {
                let exp_val = (logits_bt.add(i).read() - maxval).exp();
                probs_bt.add(i).write(exp_val);
                sum += exp_val;
            }

            // Normalize the probabilities
            for i in 0..V {
                probs_bt.add(i).write(probs_bt.add(i).read() / sum);
            }

            // Set padded dimensions to zero
            for i in V..Vp {
                probs_bt.add(i).write(0.0);
            }
        });
    });
}

/// Computes the cross-entropy losses from probabilities and targets.
///
/// # Arguments
///
/// * `losses` - Output losses (B, T).
/// * `probs` - Input probabilities (B, T, Vp).
/// * `targets` - Target indices (B, T).
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `Vp` - Padded vocabulary size.
pub unsafe fn crossentropy_forward(
    losses: *mut f32,
    probs: *const f32,
    targets: *const i32,
    B: usize,
    T: usize,
    Vp: usize,
) {
    for b in 0..B {
        for t in 0..T {
            // Calculate the base address for probs
            let probs_bt = probs.add(b * T * Vp + t * Vp);

            // Get the target index
            let ix = *targets.add(b * T + t) as usize;

            // Compute the cross-entropy loss and store it
            *losses.add(b * T + t) = -probs_bt.add(ix).read().ln();
        }
    }
}

/// Backward pass through both softmax and cross-entropy loss.
///
/// # Arguments
///
/// * `dlogits` - Gradient of the logits (B, T, Vp).
/// * `dlosses` - Gradient of the losses (B, T).
/// * `probs` - Probabilities (B, T, Vp).
/// * `targets` - Target indices (B, T).
/// * `B` - Batch size.
/// * `T` - Sequence length.
/// * `V` - Real vocabulary size.
/// * `Vp` - Padded vocabulary size.
pub unsafe fn crossentropy_softmax_backward(
    dlogits: *mut f32,
    dlosses: *const f32,
    probs: *const f32,
    targets: *const i32,
    B: usize,
    T: usize,
    V: usize,
    Vp: usize,
) {
    for b in 0..B {
        for t in 0..T {
            // Calculate the base addresses
            let dlogits_bt = dlogits.add(b * T * Vp + t * Vp);
            let probs_bt = probs.add(b * T * Vp + t * Vp);
            let dloss = *dlosses.add(b * T + t);
            let ix = *targets.add(b * T + t) as usize;

            // Loop only to V, leaving padded dimensions untouched
            for i in 0..V {
                let p = *probs_bt.add(i);
                let indicator = if i == ix { 1.0 } else { 0.0 };
                *dlogits_bt.add(i) += (p - indicator) * dloss;
            }
        }
    }
}