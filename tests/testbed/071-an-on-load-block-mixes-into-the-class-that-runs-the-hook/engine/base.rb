module Engine
  module Querying
    def find(id)
    end
  end

  class Base
    extend Querying
  end

  ActiveSupport.run_load_hooks(:engine, Base)
end

class Controller
  ActiveSupport.run_load_hooks(:controller, self)
end
