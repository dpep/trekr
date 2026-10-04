module Store
  module Dumper; end

  module Adapters
    class Dumper < Dumper
    end
  end
end

class Parent
  class Pool
    def drain; end
  end
end

class Child < Parent
  class Pool < Pool
  end
end

class Kid < Parent
  class Spare < Pool
  end
end
