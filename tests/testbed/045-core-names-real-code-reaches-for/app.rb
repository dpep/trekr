class Widget
  def run
    limit = Float::INFINITY
    started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    Thread::Mutex.new.synchronize { limit }
    Time.utc(2020).year
    File.join(__dir__, "x")
  rescue Errno::ENOTCONN
    started
  end

  protected_instance_methods
end
