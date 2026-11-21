class KF
  def self.all
    obj = Object.new
    def obj.kinds
      descendants
    end
    obj
  end
end
class KF1 < KF; end

class KG
  def self.all
    obj = Object.new
    class << obj
      def kinds
        descendants
      end
    end
    obj
  end
end
class KG1 < KG; end

class KD
  def all
    obj = Object.new
    def obj.kinds
      self.class.descendants
    end
    obj
  end
end
class KD1 < KD; end

class Printer
  def self.run(out)
    io = Object.new
    class << io
      def write(x)
        helper_one
      end
    end
    def out.flush
      helper_two
      self.helper_two
    end
    io.write(1)
  end

  def self.helper_one
    1
  end

  def self.helper_two
    2
  end
end

KF.all; KG.all; KD.new.all; Printer.run($stdout)
