module Mimer
  class Db
    if ::File.method_defined?(:pread)
      PReadFile = ::File
    else
      class PReadFile
        def initialize(filename); end
      end
    end
  end
end
