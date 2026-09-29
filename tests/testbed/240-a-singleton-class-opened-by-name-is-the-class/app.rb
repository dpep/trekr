class Stamp
  class << Stamp
    FORMAT = "%F"

    def parse(text)
      build(text)
    end

    def build(text)
    end
  end
end

class Job
  def run
    Stamp.parse("x")
  end
end
