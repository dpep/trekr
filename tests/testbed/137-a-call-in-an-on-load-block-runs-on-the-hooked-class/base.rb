module ActiveSupport
  def self.on_load(name, options = {}, &block); end
  def self.run_load_hooks(name, base = Object); end
end

module ActiveRecord
  class Base
    def self.establish(config); end
    def save; end

    ActiveSupport.run_load_hooks(:active_record, self)
  end
end

module ActionController
  class Base
    def self.helper(name); end
    ActiveSupport.run_load_hooks(:action_controller, self)
  end

  class API
    def self.helper(name); end
    ActiveSupport.run_load_hooks(:action_controller, self)
  end
end
