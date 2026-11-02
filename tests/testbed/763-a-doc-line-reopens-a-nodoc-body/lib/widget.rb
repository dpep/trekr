module Widget
  module Loader # :nodoc:
    singleton_class.attr_accessor :lock

    # :doc:

    # Run the block while holding the lock.
    def self.run_locked(&block)
      lock.synchronize(&block)
    end

    def self.unload_locked
    end

    # :nodoc:

    # Internal bookkeeping.
    def self.paths
    end

    class Entry
      # :doc:
    end

    def self.reset
    end
  end

  module Registry # :nodoc:
    # :doc:
    def self.register(name)
    end

    def self.lookup(name)
    end
  end

  module Cache # :nodoc:
    def self.fetch(key) # :doc:
    end

    def self.store(key)
    end
  end
end
