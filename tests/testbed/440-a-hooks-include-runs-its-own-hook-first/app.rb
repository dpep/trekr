module Queueing
  module Options
    def self.included(base)
      base.extend(ClassMethods)
    end

    module ClassMethods
      def configure(opts = {})
      end
    end
  end

  def self.included(base)
    base.include(Options)
    base.extend(ClassMethods)
  end

  module ClassMethods
    def configure(opts = {})
    end
  end
end

class Job
  include Queueing
  configure queue: "low"
end
