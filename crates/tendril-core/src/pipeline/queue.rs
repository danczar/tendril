use std::collections::VecDeque;

use crate::pipeline::job::{Job, JobSource};

/// Sequential FIFO queue of processing jobs.
pub struct JobQueue {
    jobs: VecDeque<Job>,
}

impl JobQueue {
    pub fn new() -> Self {
        Self {
            jobs: VecDeque::new(),
        }
    }

    /// Add a job to the end of the queue.
    pub fn enqueue(&mut self, source: JobSource) -> u64 {
        let job = Job::new(source);
        let id = job.id;
        self.jobs.push_back(job);
        id
    }

    /// Remove a job by ID. Returns `true` if it was found and removed.
    pub fn remove(&mut self, id: u64) -> bool {
        if let Some(pos) = self.jobs.iter().position(|j| j.id == id) {
            self.jobs.remove(pos);
            true
        } else {
            false
        }
    }

    /// Take the next job from the front of the queue.
    pub fn pop_front(&mut self) -> Option<Job> {
        self.jobs.pop_front()
    }

    /// Iterate over all jobs in queue order.
    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter()
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

impl Default for JobQueue {
    fn default() -> Self {
        Self::new()
    }
}
