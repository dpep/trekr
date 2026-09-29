class FromLocal
  src = "def from_var; end"
  class_eval(src)
end

class FromJoin
  class_eval(["def from_join", "end"].join("\n"))
end

class FromFormat
  class_eval format("def %s; end", "from_fmt")
end

class FromStrip
  class_eval <<~CODE.strip
    def from_strip
    end
  CODE
end

class FromGsub
  class_eval <<~CODE.gsub("XX", "made")
    def from_XX
    end
  CODE
end

class FromInstanceEval
  instance_eval "def self.from_ie; end"
end

class FromEval
  eval "def from_eval; end"
end

class Target
end

class Sender
  Target.class_eval "def from_sent; end"
end

class Plain
  def setup
    self.class.class_eval "def from_inst; end"
  end
end

FromStrip.new.from_strip
