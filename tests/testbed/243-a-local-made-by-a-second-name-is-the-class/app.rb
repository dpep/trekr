class Cache
  def fetch
    lock = Mutex.new
    lock.synchronize { 1 }
    @guard = Mutex.new
    @guard.synchronize { 2 }
  end
end
